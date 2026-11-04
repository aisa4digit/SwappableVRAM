/* Optional, bounded broker path. No dispatcher waits and no required CUDA
 * dependency: no available broker GET => immediate disk forwarding; an issued
 * request has an independent kernel timer fallback. Source prototype only. */
#include "filter.h"

EVT_WDF_TIMER SvDeadline;
EVT_WDF_IO_QUEUE_IO_CANCELED_ON_QUEUE SvBrokerGetCancelled;
EVT_WDF_REQUEST_CANCEL SvReadCancelled;
EVT_WDF_IO_QUEUE_IO_READ SvFallbackRead;
static ULONGLONG SvNow(void)
{
    ULONGLONG qpc;
    return KeQueryInterruptTimePrecise(&qpc);
}

static BOOLEAN SvSameRange(const SV_RANGE_STAMP *A, const SV_RANGE_STAMP *B)
{
    return A->Pages > 0 && A->Pages <= SV_RANGE_PAGES && RtlCompareMemory(A,B,sizeof(*A)) == sizeof(*A);
}
static PUCHAR SvRequestBuffer(WDFREQUEST Request, BOOLEAN IsWrite, ULONGLONG Length)
{
    PMDL mdl = NULL;
    NTSTATUS status = IsWrite ? WdfRequestRetrieveInputWdmMdl(Request, &mdl) : WdfRequestRetrieveOutputWdmMdl(Request, &mdl);
    /* Only a first MDL covering the whole request is used. Short descriptors
     * (including a chain) safely bypass; never copy beyond a mapped MDL. */
    if (!NT_SUCCESS(status) || mdl == NULL || Length > MAXULONG || MmGetMdlByteCount(mdl) < Length) return NULL;
    return (PUCHAR)MmGetSystemAddressForMdlSafe(mdl, HighPagePriority | MdlMappingNoExecute);
}
static void SvArmTimer(WDFDEVICE Device)
{
    SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
    ULONGLONG earliest = MAXULONGLONG;
    ULONGLONG now;
    ULONG i;
    WdfSpinLockAcquire(context->Lock);
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        if (context->Pending[i].Request != NULL && !context->Pending[i].CancelPending && context->Pending[i].State.Deadline < earliest)
            earliest = context->Pending[i].State.Deadline;
    }
    if (context->InD0 && context->CacheOperational && earliest != MAXULONGLONG) {
        now=SvNow();
        LONGLONG due = -(LONGLONG)(earliest > now ? earliest - now : 1);
        WdfTimerStart(context->DeadlineTimer, due);
    }
    WdfSpinLockRelease(context->Lock);
}
static void SvLeaseRelease(WDFDEVICE Device)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    WdfSpinLockAcquire(context->Lock);
    NT_ASSERT(context->CacheLeases > 0);
    --context->CacheLeases;
    if (context->CacheLeases == 0) KeSetEvent(&context->CacheDrained,IO_NO_INCREMENT,FALSE);
    WdfSpinLockRelease(context->Lock);
    WdfObjectDereference(Device);
}
static BOOLEAN SvCacheEnter(WDFDEVICE Device)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    BOOLEAN entered=FALSE;
    WdfSpinLockAcquire(context->Lock);
    if (context->CacheEnabled && context->CacheOperational && context->InD0) {
        WdfObjectReference(Device);
        if (context->CacheLeases++ == 0) KeClearEvent(&context->CacheDrained);
        entered=TRUE;
    }
    WdfSpinLockRelease(context->Lock);
    return entered;
}
void SvFallbackRead(WDFQUEUE Queue, WDFREQUEST Request, size_t Length)
{
    UNREFERENCED_PARAMETER(Length);
    /* This is a different queue, not WdfRequestRequeue into a parallel source.
     * It never tries the cache again and uses the ordinary asynchronous path. */
    if (WdfRequestIsCanceled(Request)) {
        InterlockedIncrement64(&SvDeviceContext(WdfIoQueueGetDevice(Queue))->CancelledReads);
        WdfRequestComplete(Request,STATUS_CANCELLED);
    } else SvSendTracked(WdfIoQueueGetDevice(Queue),Request);
}
static void SvFallback(WDFDEVICE Device, WDFREQUEST Request)
{
    NTSTATUS status=WdfRequestForwardToIoQueue(Request,SvDeviceContext(Device)->FallbackQueue);
    if (!NT_SUCCESS(status)) WdfRequestComplete(Request,status);
    /* Cache ownership ends at handoff, not at lower-disk completion. */
    SvLeaseRelease(Device);
}
/* Caller holds the device lock. Unmark never transfers a CANCELLED request to
 * this path: its cancel callback is/will be the sole owner. MarkCancelableEx
 * (not legacy MarkCancelable) and the same lock serialize registration too.
 * Microsoft SPB SkeletonI2C uses lock-held MarkCancelableEx; see driver docs. */
static WDFREQUEST SvClaim(SV_PENDING *Slot, BOOLEAN Cancel)
{
    WDFREQUEST request;
    NTSTATUS status;
    if (Slot->Request == NULL || Slot->CancelPending) return NULL;
    status=WdfRequestUnmarkCancelable(Slot->Request);
    if (status == STATUS_CANCELLED) { Slot->CancelPending=TRUE; return NULL; }
    if (!NT_SUCCESS(status)) return NULL;
    if (!(Cancel ? sv_request_claim_cancel(&Slot->State) : sv_request_claim_disk(&Slot->State))) return NULL;
    request=Slot->Request; Slot->Request=NULL;
    return request;
}
void SvReadCancelled(WDFREQUEST Request)
{
    WDFDEVICE device=WdfIoQueueGetDevice(WdfRequestGetIoQueue(Request));
    SV_DEVICE_CONTEXT *context=SvDeviceContext(device);
    BOOLEAN owned=FALSE;
    ULONG i;
    WdfSpinLockAcquire(context->Lock);
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        SV_PENDING *slot=&context->Pending[i];
        if (slot->Request == Request) {
            if (sv_request_claim_cancel(&slot->State)) {
                slot->Request=NULL; owned=TRUE;
            }
            break;
        }
    }
    WdfSpinLockRelease(context->Lock);
    if (owned) {
        InterlockedIncrement64(&context->CancelledReads);
        WdfRequestComplete(Request,STATUS_CANCELLED);
        SvLeaseRelease(device);
    }
}
void SvDeadline(WDFTIMER Timer)
{
    WDFDEVICE device = (WDFDEVICE)WdfTimerGetParentObject(Timer);
    SV_DEVICE_CONTEXT *context = SvDeviceContext(device);
    WDFREQUEST forward[SV_PENDING_LIMIT];
    ULONG count = 0, i;
    ULONGLONG now;
    WdfSpinLockAcquire(context->Lock);
    now=SvNow();
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        SV_PENDING *slot = &context->Pending[i];
        if (slot->Request != NULL && slot->State.Deadline <= now) {
            WDFREQUEST request=SvClaim(slot,FALSE);
            if (request != NULL) {
                InterlockedIncrement64(&context->DeadlineFallbacks);
                forward[count++]=request;
            }
        }
    }
    WdfSpinLockRelease(context->Lock);
    for (i=0; i<count; ++i) SvFallback(device, forward[i]);
    SvArmTimer(device);
}
void SvBrokerGetCancelled(WDFQUEUE Queue, WDFREQUEST Request)
{
    InterlockedDecrement(&SvDeviceContext(SvControlTarget(WdfIoQueueGetDevice(Queue)))->BrokerWaiting);
    WdfRequestComplete(Request, STATUS_CANCELLED);
}
NTSTATUS SvCacheInitialize(WDFDEVICE Device)
{
    SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
    WDF_IO_QUEUE_CONFIG queue;
    WDF_TIMER_CONFIG timer;
    WDF_OBJECT_ATTRIBUTES attributes;
    NTSTATUS status;
    KeInitializeEvent(&context->CacheDrained,NotificationEvent,TRUE);
    WDF_IO_QUEUE_CONFIG_INIT(&queue,WdfIoQueueDispatchParallel);
    queue.PowerManaged=WdfFalse;
    queue.EvtIoRead=SvFallbackRead;
    status=WdfIoQueueCreate(Device,&queue,WDF_NO_OBJECT_ATTRIBUTES,&context->FallbackQueue);
    if (!NT_SUCCESS(status)) return status;
    WDF_TIMER_CONFIG_INIT(&timer, SvDeadline);
    timer.AutomaticSerialization = FALSE;
    timer.UseHighResolutionTimer = WdfTrue;
    timer.TolerableDelay = 0;
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes); attributes.ParentObject = Device;
    status = WdfTimerCreate(&timer, &attributes, &context->DeadlineTimer);
    if (!NT_SUCCESS(status)) return status;
    return STATUS_SUCCESS;
}
void SvCachePowerBoundary(WDFDEVICE Device, BOOLEAN InD0)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    SV_SESSION *owner;
    WdfSpinLockAcquire(context->Lock);
    context->CacheEnabled=0;
    if (!InD0) context->CacheOperational=0;
    owner=context->BrokerSession; context->BrokerSession=NULL;
    context->InD0=InD0;
    if (context->BrokerEpoch != MAXULONGLONG) ++context->BrokerEpoch;
    sv_book_invalidate_all(&context->Book);
    WdfSpinLockRelease(context->Lock);
    SvSessionRelease(owner);
    /* No wait, GPU call or re-enable. Old replies fail generation validation;
     * already-owned reads still have the independent deadline/lower path. */
}
void SvCacheResume(WDFDEVICE Device)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    WdfSpinLockAcquire(context->Lock);
    context->CacheOperational=context->InD0;
    WdfSpinLockRelease(context->Lock);
    /* No automatic broker/cache re-enable after a power/resource transition. */
}
void SvCacheQuiesce(WDFDEVICE Device, BOOLEAN Purge)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    WDFREQUEST requests[SV_PENDING_LIMIT];
    ULONG count=0,i;
    SvCachePowerBoundary(Device,FALSE);
    InterlockedIncrement64(&context->QuiesceCalls);
    /* These callbacks run at PASSIVE_LEVEL. ArmTimer cannot restart once the
     * gate closes, so this waits only for our bounded CPU-only timer callback. */
    (void)WdfTimerStop(context->DeadlineTimer,TRUE);
    WdfSpinLockAcquire(context->Lock);
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        WDFREQUEST request=SvClaim(&context->Pending[i],Purge);
        if (request != NULL) requests[count++]=request;
    }
    WdfSpinLockRelease(context->Lock);
    for (i=0; i<count; ++i) {
        if (Purge) {
            InterlockedIncrement64(&context->CancelledReads);
            WdfRequestComplete(requests[i],STATUS_CANCELLED);
            SvLeaseRelease(Device);
        } else SvFallback(Device,requests[i]);
    }
    for (i=0; i<32; ++i) {
        WDFREQUEST get;
        if (context->BrokerQueue == NULL || !NT_SUCCESS(WdfIoQueueRetrieveNextRequest(context->BrokerQueue,&get))) break;
        InterlockedDecrement(&context->BrokerWaiting);
        WdfRequestComplete(get,STATUS_CANCELLED);
    }
    /* Only cache CPU/cancel callbacks and queue handoffs hold leases; ordinary
     * lower-disk completions do not. No GPU/user process/disk completion wait.
     * No arbitrary timeout can falsely authorize teardown of outstanding work. */
    (void)KeWaitForSingleObject(&context->CacheDrained,Executive,KernelMode,FALSE,NULL);
}
static WDFREQUEST SvTakeGet(SV_DEVICE_CONTEXT *Context, SV_FRAME **Frame, ULONGLONG *Epoch)
{
    WDFREQUEST request;
    NTSTATUS status = WdfIoQueueRetrieveNextRequest(Context->BrokerQueue, &request);
    if (!NT_SUCCESS(status)) return NULL;
    InterlockedDecrement(&Context->BrokerWaiting);
    status = WdfRequestRetrieveOutputBuffer(request, sizeof(SV_FRAME), (PVOID *)Frame, NULL);
    if (!NT_SUCCESS(status)) { WdfRequestComplete(request,status); return NULL; }
    *Epoch=SvRequestContext(request)->BrokerEpoch;
    return request;
}
static BOOLEAN SvBuildFrame(SV_DEVICE_CONTEXT *Context, SV_FRAME *Frame, ULONG Operation, const SV_RANGE_STAMP *Range)
{
    if (Context->NextSequence == MAXULONGLONG) { Context->CacheEnabled = 0; return FALSE; }
    /* READ/MISS are header-only; no empty 64 KiB copy or payload leak. */
    RtlZeroMemory(Frame,SV_FRAME_HEADER); Frame->Magic = SV_FRAME_MAGIC; Frame->Version = SV_PROTOCOL_VERSION;
    Frame->Operation = Operation; Frame->Range = *Range; Frame->Sequence = ++Context->NextSequence;
    Frame->Length=Range->Pages*4096;
    if (Operation == SV_OP_READ) {
        ULONGLONG now=SvNow();
        if (now>MAXULONGLONG-10000) { Context->CacheEnabled=0; return FALSE; }
        Frame->Deadline=now+10000;
    }
    return TRUE;
}
BOOLEAN SvCacheTryRead(WDFDEVICE Device, WDFREQUEST Request)
{
    SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
    SV_REQUEST_CONTEXT *original = SvRequestContext(Request);
    WDFREQUEST get;
    SV_FRAME *frame;
    SV_RANGE_STAMP current;
    ULONG i;
    ULONGLONG brokerEpoch;
    BOOLEAN issued = FALSE;
    BOOLEAN markAttempted = FALSE;
    NTSTATUS markStatus = STATUS_RETRY;
    if (WdfRequestIsCanceled(Request) || !SvCacheEnter(Device)) return FALSE;
    get = SvTakeGet(context,&frame,&brokerEpoch);
    if (get == NULL) { SvLeaseRelease(Device); return FALSE; }
    WdfSpinLockAcquire(context->Lock);
    if (context->CacheEnabled && context->BrokerEpoch == brokerEpoch && sv_book_range_stamp(&context->Book,original->Offset,original->Length,&current)) {
        for (i=0; i<SV_PENDING_LIMIT; ++i) {
            SV_PENDING *slot = &context->Pending[i];
            if (slot->Request == NULL) {
                if (SvBuildFrame(context,frame,SV_OP_READ,&current)) {
                    original->ReadRange = current; original->ReadStampValid = TRUE;
                    slot->Request = Request; slot->Range = current;
                    slot->CancelPending = FALSE;
                    sv_request_prepare(&slot->State,frame->Sequence,current.Generations[0],frame->Deadline);
                    markAttempted=TRUE;
                    markStatus=WdfRequestMarkCancelableEx(Request,SvReadCancelled);
                    if (NT_SUCCESS(markStatus)) {
                        WdfObjectReference(Device);
                        if (context->CacheLeases++ == 0) KeClearEvent(&context->CacheDrained);
                        issued=TRUE; InterlockedIncrement64(&context->IssuedReads);
                    }
                    else { sv_request_claim_cancel(&slot->State); slot->Request=NULL; }
                }
                break;
            }
        }
    }
    WdfSpinLockRelease(context->Lock);
    if (!issued) {
        WdfRequestComplete(get,STATUS_RETRY);
        SvLeaseRelease(Device);
        if (markAttempted) {
            /* Ex does not invoke the callback when registration returns
             * STATUS_CANCELLED. This path still owns and completes the read. */
            if (markStatus == STATUS_CANCELLED) InterlockedIncrement64(&context->CancelledReads);
            WdfRequestComplete(Request,markStatus); return TRUE;
        }
        return FALSE;
    }
    SvArmTimer(Device);
    /* Original request can finish on another CPU after this completion. Do not
     * access it again. It remains driver-owned until hit or timer claims it. */
    WdfRequestCompleteWithInformation(get,STATUS_SUCCESS,SV_FRAME_HEADER);
    SvLeaseRelease(Device);
    return TRUE;
}
void SvCacheOffer(WDFDEVICE Device, WDFREQUEST Request, const SV_STAMP *WriteStamp)
{
    SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
    SV_REQUEST_CONTEXT *original = SvRequestContext(Request);
    SV_FRAME *frame;
    WDFREQUEST get;
    PUCHAR source;
    SV_RANGE_STAMP current;
    ULONGLONG position;
    ULONG slices=0;
    BOOLEAN isWrite=(BOOLEAN)(WriteStamp != NULL);
    ULONGLONG brokerEpoch;
    if (!SvCacheEnter(Device)) return;
    if (original->Length == 0 || original->Length % 4096 != 0 || original->Offset % 4096 != 0) { SvLeaseRelease(Device); return; }
    source = SvRequestBuffer(Request,isWrite,original->Length);
    if (source == NULL) { SvLeaseRelease(Device); return; }
    /* At most 1 MiB / 16 slices; absent GETs drop optional fills immediately.
     * Every slice is checked against the completed write's generation, so a
     * later overlapping write cannot relabel stale payload with a new stamp. */
    for (position=0; position<original->Length && slices<16; position+=SV_RANGE_BYTES,++slices) {
        ULONG length=(ULONG)min((ULONGLONG)SV_RANGE_BYTES,original->Length-position), i;
        BOOLEAN issued=FALSE, valid=FALSE;
        get=SvTakeGet(context,&frame,&brokerEpoch); if (get == NULL) break;
        WdfSpinLockAcquire(context->Lock);
        if (context->CacheEnabled && context->BrokerEpoch == brokerEpoch &&
            sv_book_range_stamp(&context->Book,original->Offset+position,length,&current)) {
            if (isWrite) {
                valid=(BOOLEAN)(current.VolumeEpoch == WriteStamp->VolumeEpoch &&
                    current.PageIndex == WriteStamp->PageIndex+position/4096);
                for (i=0; valid && i<current.Pages; ++i) valid=(BOOLEAN)(current.Generations[i] == WriteStamp->Generation);
            } else valid=SvSameRange(&current,&original->ReadRange);
            if (valid && SvBuildFrame(context,frame,SV_OP_FILL,&current)) {
                issued=TRUE;
            }
        }
        WdfSpinLockRelease(context->Lock);
        if (issued) {
            /* Original I/O and retrieved GET remain driver-owned. Copy outside
             * the device-wide lock, then reject any generation/epoch change
             * that raced the copy before publishing its immutable stamp. */
            RtlCopyMemory(frame->Data,source+(SIZE_T)position,length);
            WdfSpinLockAcquire(context->Lock);
            issued=(BOOLEAN)(context->CacheEnabled && context->BrokerEpoch == brokerEpoch &&
                sv_book_range_stamp(&context->Book,original->Offset+position,length,&current) &&
                SvSameRange(&frame->Range,&current));
            WdfSpinLockRelease(context->Lock);
        }
        if (issued) InterlockedIncrement64(&context->OfferedFills);
        WdfRequestCompleteWithInformation(get,issued ? STATUS_SUCCESS : STATUS_RETRY,issued ? SV_FRAME_HEADER+length : 0);
    }
    SvLeaseRelease(Device);
}
static NTSTATUS SvDisable(WDFDEVICE Device, SV_SESSION *ExpectedSession)
{
    SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
    SV_SESSION *owner;
    WDFREQUEST forward[SV_PENDING_LIMIT]; ULONG count=0, i;
    WdfSpinLockAcquire(context->Lock);
    if (context->Disabling) { WdfSpinLockRelease(context->Lock); return STATUS_DEVICE_BUSY; }
    if (context->BrokerSession != NULL && context->BrokerSession != ExpectedSession) {
        WdfSpinLockRelease(context->Lock); return STATUS_ACCESS_DENIED;
    }
    context->CacheEnabled=0;
    owner=context->BrokerSession; context->BrokerSession=NULL;
    context->Disabling=1;
    if (context->BrokerEpoch != MAXULONGLONG) ++context->BrokerEpoch;
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        SV_PENDING *slot=&context->Pending[i];
        WDFREQUEST request=SvClaim(slot,FALSE);
        if (request != NULL) forward[count++]=request;
    }
    WdfSpinLockRelease(context->Lock);
    /* Drain, rather than stop/purge/start the queue. A future owner cannot
     * receive old queued GETs or race WdfIoQueueStart against an async purge. */
    for (i=0; i<32; ++i) {
        WDFREQUEST get;
        if (!NT_SUCCESS(WdfIoQueueRetrieveNextRequest(context->BrokerQueue,&get))) break;
        InterlockedDecrement(&context->BrokerWaiting);
        WdfRequestComplete(get,STATUS_CANCELLED);
    }
    for (i=0; i<count; ++i) SvFallback(Device,forward[i]);
    WdfSpinLockAcquire(context->Lock); context->Disabling=0; WdfSpinLockRelease(context->Lock);
    SvSessionRelease(owner);
    return STATUS_SUCCESS;
}
void SvBrokerFileCleanup(WDFFILEOBJECT FileObject)
{
    WDFDEVICE control=WdfFileObjectGetDevice(FileObject);
    WDFDEVICE device;
    SV_SESSION *caller=SvFileContext(FileObject)->Session;
    SV_DEVICE_CONTEXT *context;
    BOOLEAN owner;
    if (caller == NULL) return;
    /* Close the gate before taking the parent lock. ENABLE checks this gate
     * under that same lock, so cleanup-before-enable cannot leave a dead owner. */
    InterlockedExchange(&caller->Closing,1);
    if (!SvControlEnter(control)) return;
    device=SvControlTarget(control); context=SvDeviceContext(device);
    WdfSpinLockAcquire(context->Lock);
    owner=(BOOLEAN)(context->BrokerSession == caller);
    WdfSpinLockRelease(context->Lock);
    /* SvDisable rechecks the token; an old cleanup cannot disable a new owner. */
    if (owner) (void)SvDisable(device,caller);
    SvControlLeave(control);
}
static NTSTATUS SvReply(WDFDEVICE Device, const SV_FRAME *Frame, size_t InputLength)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(Device);
    WDFREQUEST original=NULL; BOOLEAN hit=FALSE; ULONG i; SV_RANGE_STAMP current={0};
    PUCHAR destination=NULL;
    if (Frame->Magic != SV_FRAME_MAGIC || Frame->Version != SV_PROTOCOL_VERSION || Frame->Operation != SV_OP_READ || Frame->Deadline == 0 ||
        Frame->Length == 0 || Frame->Length > SV_RANGE_BYTES || Frame->Length % 4096 != 0 ||
        Frame->Range.Pages != Frame->Length/4096 || Frame->Range.Reserved != 0 ||
        Frame->Range.PageIndex > (MAXULONGLONG-Frame->Length)/4096 ||
        (Frame->Status != SV_STATUS_HIT && Frame->Status != SV_STATUS_MISS) ||
        InputLength != SV_FRAME_HEADER+(Frame->Status == SV_STATUS_HIT ? Frame->Length : 0)) return STATUS_INVALID_PARAMETER;
    for (i=0; i<SV_RANGE_PAGES; ++i) {
        if ((i<Frame->Range.Pages) != (Frame->Range.Generations[i] != 0)) return STATUS_INVALID_PARAMETER;
    }
    WdfSpinLockAcquire(context->Lock);
    for (i=0; i<SV_PENDING_LIMIT; ++i) {
        SV_PENDING *slot=&context->Pending[i];
        if (slot->Request != NULL && slot->State.Sequence == Frame->Sequence) {
            NTSTATUS status;
            if (slot->CancelPending) break;
            status=WdfRequestUnmarkCancelable(slot->Request);
            if (status == STATUS_CANCELLED) { slot->CancelPending=TRUE; break; }
            if (!NT_SUCCESS(status)) break;
            if (Frame->Status == SV_STATUS_HIT && Frame->Deadline == slot->State.Deadline && SvSameRange(&slot->Range,&Frame->Range) &&
                sv_book_range_stamp(&context->Book,SvRequestContext(slot->Request)->Offset,SvRequestContext(slot->Request)->Length,&current) &&
                SvSameRange(&slot->Range,&current) && !WdfRequestIsCanceled(slot->Request))
                destination=SvRequestBuffer(slot->Request,FALSE,Frame->Length);
            if (destination != NULL && sv_request_claim_hit(&slot->State,Frame->Sequence,current.Generations[0],SvNow())) {
                hit=TRUE;
            } else if (!sv_request_claim_disk(&slot->State)) break;
            original=slot->Request; slot->Request=NULL; break;
        }
    }
    WdfSpinLockRelease(context->Lock);
    if (original == NULL) { InterlockedIncrement64(&context->LateReplies); return STATUS_NOT_FOUND; }
    if (hit) {
        RtlCopyMemory(destination,Frame->Data,Frame->Length);
        InterlockedIncrement64(&context->CacheHits);
        WdfRequestCompleteWithInformation(original,STATUS_SUCCESS,Frame->Length);
        SvLeaseRelease(Device);
    }
    else { InterlockedIncrement64(&context->ReplyFallbacks); SvFallback(Device,original); }
    return STATUS_SUCCESS;
}
BOOLEAN SvCacheControl(WDFDEVICE device, WDFREQUEST Request, size_t OutputLength, ULONG Code)
{
    SV_DEVICE_CONTEXT *context=SvDeviceContext(device);
    ULONG pid=IoGetRequestorProcessId(WdfRequestWdmGetIrp(Request));
    SV_SESSION *session;
    BOOLEAN owner;
    NTSTATUS status=STATUS_INVALID_DEVICE_REQUEST;
    SV_ENABLE *enable;
    SV_FRAME *reply;
    size_t inputLength;
    if (Code != IOCTL_SVRAM_ENABLE && Code != IOCTL_SVRAM_GET && Code != IOCTL_SVRAM_REPLY) return FALSE;
    if (!SV_ENABLE_EXPERIMENTAL_CACHE) { WdfRequestComplete(Request,STATUS_NOT_SUPPORTED); return TRUE; }
    session=SvRequestContext(Request)->CallerSession;
    if (session == NULL) { WdfRequestComplete(Request,STATUS_ACCESS_DENIED); return TRUE; }
    WdfSpinLockAcquire(context->Lock);
    owner=(BOOLEAN)(!InterlockedCompareExchange(&session->Closing,0,0) && context->CacheEnabled && context->BrokerProcessId == pid && context->BrokerSession == session);
    WdfSpinLockRelease(context->Lock);
    if (Code == IOCTL_SVRAM_ENABLE) {
        status=WdfRequestRetrieveInputBuffer(Request,sizeof(*enable),(PVOID *)&enable,NULL);
        if (NT_SUCCESS(status)) {
            if (enable->Size != sizeof(*enable) || enable->Version != SV_PROTOCOL_VERSION || enable->Reserved != 0 || enable->Enabled > 1) status=STATUS_INVALID_PARAMETER;
            else if (enable->Enabled) {
                WdfSpinLockAcquire(context->Lock);
                if (InterlockedCompareExchange(&session->Closing,0,0)) status=STATUS_FILE_CLOSED;
                else if (!context->InD0 || !context->CacheOperational) status=STATUS_DEVICE_NOT_READY;
                else if (context->Disabling) status=STATUS_DEVICE_BUSY;
                else if (context->CacheEnabled && (context->BrokerProcessId != pid || context->BrokerSession != session)) status=STATUS_ACCESS_DENIED;
                else if (context->BrokerEpoch == MAXULONGLONG) status=STATUS_INTEGER_OVERFLOW;
                else if (!context->CacheEnabled) {
                    if (context->BrokerSession != NULL) status=STATUS_DEVICE_BUSY;
                    else {
                        ++context->BrokerEpoch; context->BrokerProcessId=pid;
                        SvSessionReference(session); context->BrokerSession=session; context->CacheEnabled=1;
                    }
                }
                WdfSpinLockRelease(context->Lock);
            } else {
                status=SvDisable(device,session);
            }
        }
    } else if (!owner) status=STATUS_ACCESS_DENIED;
    else if (Code == IOCTL_SVRAM_GET) {
        if (OutputLength < sizeof(SV_FRAME)) status=STATUS_BUFFER_TOO_SMALL;
        else if (InterlockedIncrement(&context->BrokerWaiting) > 32) { InterlockedDecrement(&context->BrokerWaiting); status=STATUS_DEVICE_BUSY; }
        else {
            WdfSpinLockAcquire(context->Lock);
            if (InterlockedCompareExchange(&session->Closing,0,0) || !context->CacheEnabled || context->BrokerProcessId != pid || context->BrokerSession != session) status=STATUS_ACCESS_DENIED;
            else {
                SvRequestContext(Request)->BrokerEpoch=context->BrokerEpoch;
                status=WdfRequestForwardToIoQueue(Request,context->BrokerQueue);
            }
            WdfSpinLockRelease(context->Lock);
            if (NT_SUCCESS(status)) return TRUE;
            InterlockedDecrement(&context->BrokerWaiting);
        }
    } else {
        status=WdfRequestRetrieveInputBuffer(Request,SV_FRAME_HEADER,(PVOID *)&reply,&inputLength);
        if (NT_SUCCESS(status)) status=SvReply(device,reply,inputLength);
    }
    WdfRequestComplete(Request,status); return TRUE;
}
