/*
 * SwappableVRAM KMDF storage upper-filter development bridge.
 *
 * Default build: BYPASS ONLY. The explicitly experimental build also contains
 * an optional broker path; it is NOT installed or pagefile-qualified.
 * The allocation-free Rust generation book is wired into actual write dispatch
 * and completion. It is the coherence boundary for the forthcoming optional
 * CUDA read cache. This binary is NOT pagefile-qualified or installable yet.
 * No code or data on the I/O path is placed into pageable sections.
 */
#include "filter.h"

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD SvDeviceAdd;
EVT_WDF_IO_QUEUE_IO_READ SvRead;
EVT_WDF_IO_QUEUE_IO_WRITE SvWrite;
EVT_WDF_IO_QUEUE_IO_DEFAULT SvDefault;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL SvDeviceControl;
EVT_WDF_REQUEST_COMPLETION_ROUTINE SvLowerComplete;
EVT_WDF_DEVICE_USAGE_NOTIFICATION SvUsageNotification;
EVT_WDF_DEVICE_D0_ENTRY SvD0Entry;
EVT_WDF_DEVICE_D0_EXIT SvD0Exit;
EVT_WDF_DEVICE_SELF_MANAGED_IO_SUSPEND SvSelfManagedSuspend;
EVT_WDF_DEVICE_SELF_MANAGED_IO_RESTART SvSelfManagedRestart;
EVT_WDF_DEVICE_SELF_MANAGED_IO_INIT SvSelfManagedInit;
EVT_WDF_DEVICE_SELF_MANAGED_IO_FLUSH SvSelfManagedFlush;
EVT_WDF_DEVICE_SELF_MANAGED_IO_CLEANUP SvSelfManagedCleanup;
EVT_WDF_OBJECT_CONTEXT_CLEANUP SvDeviceCleanup;
static const GUID SvInterface={0x79320e2b,0x6b29,0x4df1,{0xa8,0x27,0x86,0x91,0x05,0x41,0x2c,0x47}};

/* Not executable without an installed filter device. Rust panic is fail-fast,
 * never an unbounded spin on a kernel processor. Normal book APIs do not panic. */
DECLSPEC_NORETURN void SvDriverPanic(void)
{
    /* An unexpected Rust panic cannot unwind across the kernel C ABI or return
     * safely. Recoverable GPU/cache errors do not take this path. Fail fast
     * instead of spinning a CPU forever or continuing corrupted control flow. */
#pragma warning(suppress:28159)
    KeBugCheckEx(DRIVER_VERIFIER_DETECTED_VIOLATION, 0x53565241, 0, 0, 0);
}

NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_DRIVER_CONFIG_INIT(&config, SvDeviceAdd);
    return WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES,
                          &config, WDF_NO_HANDLE);
}

NTSTATUS SvDeviceAdd(WDFDRIVER Driver, PWDFDEVICE_INIT DeviceInit)
{
    NTSTATUS status;
    WDFDEVICE device;
    WDFQUEUE queue;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDF_OBJECT_ATTRIBUTES requestAttributes;
    WDF_OBJECT_ATTRIBUTES lockAttributes;
    WDF_IO_QUEUE_CONFIG queueConfig;
    WDF_IO_QUEUE_FORWARD_PROGRESS_POLICY forwardProgress;
    WDF_PNPPOWER_EVENT_CALLBACKS pnpCallbacks;
    WDF_FILEOBJECT_CONFIG fileConfig;
    SV_DEVICE_CONTEXT *context;
    LARGE_INTEGER epoch;
    UNREFERENCED_PARAMETER(Driver);

    WdfFdoInitSetFilter(DeviceInit);
    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig,WDF_NO_EVENT_CALLBACK,WDF_NO_EVENT_CALLBACK,WDF_NO_EVENT_CALLBACK);
    fileConfig.AutoForwardCleanupClose=WdfTrue;
    fileConfig.FileObjectClass=(WDF_FILEOBJECT_CLASS)(WdfFileObjectWdfCannotUseFsContexts | WdfFileObjectCanBeOptional);
    WdfDeviceInitSetFileObjectConfig(DeviceInit,&fileConfig,WDF_NO_OBJECT_ATTRIBUTES);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&requestAttributes, SV_REQUEST_CONTEXT);
    WdfDeviceInitSetRequestAttributes(DeviceInit, &requestAttributes);
    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpCallbacks);
    pnpCallbacks.EvtDeviceUsageNotification = SvUsageNotification;
    pnpCallbacks.EvtDeviceD0Entry = SvD0Entry;
    pnpCallbacks.EvtDeviceD0Exit = SvD0Exit;
    pnpCallbacks.EvtDeviceSelfManagedIoSuspend = SvSelfManagedSuspend;
    pnpCallbacks.EvtDeviceSelfManagedIoRestart = SvSelfManagedRestart;
    pnpCallbacks.EvtDeviceSelfManagedIoInit = SvSelfManagedInit;
    pnpCallbacks.EvtDeviceSelfManagedIoFlush = SvSelfManagedFlush;
    pnpCallbacks.EvtDeviceSelfManagedIoCleanup = SvSelfManagedCleanup;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpCallbacks);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, SV_DEVICE_CONTEXT);
    attributes.ExecutionLevel = WdfExecutionLevelDispatch;
    attributes.SynchronizationScope = WdfSynchronizationScopeNone;
    attributes.EvtCleanupCallback=SvDeviceCleanup;
    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) return status;

    context = SvDeviceContext(device);
    WDF_OBJECT_ATTRIBUTES_INIT(&lockAttributes);
    lockAttributes.ParentObject = device;
    status = WdfSpinLockCreate(&lockAttributes, &context->Lock);
    if (!NT_SUCCESS(status)) return status;
    epoch = KeQueryPerformanceCounter(NULL);
    sv_book_init(&context->Book, (ULONGLONG)epoch.QuadPart);
    status = SvCacheInitialize(device);
    if (!NT_SUCCESS(status)) return status;

    /* Declaration of intent, NOT evidence of an active/qualified pagefile path. */
    WdfDeviceSetSpecialFileSupport(device, WdfSpecialFilePaging, TRUE);
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queueConfig, WdfIoQueueDispatchParallel);
    queueConfig.PowerManaged = WdfFalse;
    queueConfig.EvtIoRead = SvRead;
    queueConfig.EvtIoWrite = SvWrite;
    queueConfig.EvtIoDefault = SvDefault;
    queueConfig.EvtIoDeviceControl = SvDeviceControl;
    status = WdfIoQueueCreate(device, &queueConfig, WDF_NO_OBJECT_ATTRIBUTES, &queue);
    if (!NT_SUCCESS(status)) return status;
    /* Request contexts above are included in these preallocated reserves. All
     * other optional cache resources will have to permit immediate bypass. */
    WDF_IO_QUEUE_FORWARD_PROGRESS_POLICY_PAGINGIO_INIT(&forwardProgress, 32);
    status=WdfIoQueueAssignForwardProgressPolicy(queue, &forwardProgress);
    if (!NT_SUCCESS(status)) return status;
    status=WdfDeviceCreateDeviceInterface(device,&SvInterface,NULL);
    if (!NT_SUCCESS(status)) return status;
    return SvCreateControlDevice(device);
}

void SvUsageNotification(WDFDEVICE Device, WDF_SPECIAL_FILE_TYPE Type, BOOLEAN InPath)
{
    if (Type == WdfSpecialFilePaging) {
        SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
        if (InPath) InterlockedIncrement(&context->PagingPaths);
        else InterlockedDecrement(&context->PagingPaths);
    }
}

NTSTATUS SvD0Entry(WDFDEVICE Device, WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);
    SvCachePowerBoundary(Device,TRUE);
    return STATUS_SUCCESS;
}
NTSTATUS SvD0Exit(WDFDEVICE Device, WDF_POWER_DEVICE_STATE TargetState)
{
    UNREFERENCED_PARAMETER(TargetState);
    SvCachePowerBoundary(Device,FALSE);
    return STATUS_SUCCESS;
}
NTSTATUS SvSelfManagedInit(WDFDEVICE Device)
{
    SvCacheResume(Device); return STATUS_SUCCESS;
}
NTSTATUS SvSelfManagedRestart(WDFDEVICE Device)
{
    SvCacheResume(Device); return STATUS_SUCCESS;
}
NTSTATUS SvSelfManagedSuspend(WDFDEVICE Device)
{
    SvCacheQuiesce(Device,FALSE); return STATUS_SUCCESS;
}
void SvSelfManagedFlush(WDFDEVICE Device)
{
    SvCacheQuiesce(Device,TRUE);
}
void SvSelfManagedCleanup(WDFDEVICE Device)
{
    SvCacheQuiesce(Device,TRUE);
    SvRetireControlDevice(Device);
}
void SvDeviceCleanup(WDFOBJECT Object)
{
    /* Also handles a failed EvtDeviceAdd before self-managed I/O starts. */
    SvDeleteControlDevice((WDFDEVICE)Object);
}

static BOOLEAN SvEndWrite(WDFDEVICE Device, WDFREQUEST Request, SV_STAMP *Stamp)
{
    SV_REQUEST_CONTEXT *request = SvRequestContext(Request);
    BOOLEAN canFill = FALSE;
    if (request->IsWrite) {
        SV_DEVICE_CONTEXT *context = SvDeviceContext(Device);
        WdfSpinLockAcquire(context->Lock);
        canFill = request->WriteTicketValid &&
            sv_book_ticket_matches(&context->Book, request->Offset, request->Length, &request->WriteTicket);
        sv_book_end_write(&context->Book, request->Offset, request->Length);
        if (canFill) canFill = (BOOLEAN)sv_book_stamp(&context->Book, request->Offset, Stamp);
        WdfSpinLockRelease(context->Lock);
        request->IsWrite = FALSE;
    }
    return canFill;
}

void SvSendTracked(WDFDEVICE Device, WDFREQUEST Request)
{
    /* No SYNCHRONOUS flag, dispatcher wait, heap allocation, or user service. */
    WdfRequestFormatRequestUsingCurrentType(Request);
    WdfRequestSetCompletionRoutine(Request, SvLowerComplete, Device);
    if (!WdfRequestSend(Request, WdfDeviceGetIoTarget(Device), WDF_NO_SEND_OPTIONS)) {
        NTSTATUS status = WdfRequestGetStatus(Request);
        SV_STAMP ignored;
        SvEndWrite(Device, Request, &ignored);
        WdfRequestComplete(Request, status);
    }
}

void SvLowerComplete(WDFREQUEST Request, WDFIOTARGET Target,
                     PWDF_REQUEST_COMPLETION_PARAMS Parameters, WDFCONTEXT Context)
{
    SV_REQUEST_CONTEXT *request = SvRequestContext(Request);
    BOOLEAN wasWrite = request->IsWrite;
    SV_STAMP stamp;
    BOOLEAN writeStampValid;
    UNREFERENCED_PARAMETER(Target);
    writeStampValid = SvEndWrite((WDFDEVICE)Context, Request, &stamp);
    if (NT_SUCCESS(Parameters->IoStatus.Status) && Parameters->IoStatus.Information == request->Length && request->IsPaging) {
        if (wasWrite && writeStampValid) SvCacheOffer((WDFDEVICE)Context, Request, &stamp);
        else if (!wasWrite && request->ReadStampValid) SvCacheOffer((WDFDEVICE)Context, Request, NULL);
    }
    WdfRequestCompleteWithInformation(Request, Parameters->IoStatus.Status,
                                     Parameters->IoStatus.Information);
}

void SvRead(WDFQUEUE Queue, WDFREQUEST Request, size_t Length)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    SV_DEVICE_CONTEXT *context = SvDeviceContext(device);
    WDF_REQUEST_PARAMETERS parameters;
    PIRP irp = WdfRequestWdmGetIrp(Request);
    SV_RANGE_STAMP stamp;
    SV_REQUEST_CONTEXT *request = SvRequestContext(Request);
    request->IsWrite = FALSE;
    request->ReadStampValid = FALSE;
    request->IsPaging = (BOOLEAN)((irp->Flags & IRP_PAGING_IO) != 0);
    InterlockedIncrement64(&context->Reads);
    WDF_REQUEST_PARAMETERS_INIT(&parameters);
    WdfRequestGetParameters(Request, &parameters);
    request->Offset = (ULONGLONG)parameters.Parameters.Read.DeviceOffset;
    request->Length = (ULONGLONG)Length;
    if ((irp->Flags & IRP_PAGING_IO) != 0) {
        InterlockedIncrement64(&context->PagingReads);
        if (Length > 0 && Length <= SV_RANGE_BYTES && parameters.Parameters.Read.DeviceOffset >= 0) {
            WdfSpinLockAcquire(context->Lock);
            if (sv_book_range_stamp(&context->Book, request->Offset, request->Length, &stamp)) {
                InterlockedIncrement64(&context->Candidates);
                request->ReadRange = stamp;
                request->ReadStampValid = TRUE;
            }
            WdfSpinLockRelease(context->Lock);
        }
    }
    if (request->ReadStampValid && SvCacheTryRead(device, Request)) return;
    SvSendTracked(device, Request);
}

void SvWrite(WDFQUEUE Queue, WDFREQUEST Request, size_t Length)
{
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    SV_DEVICE_CONTEXT *context = SvDeviceContext(device);
    SV_REQUEST_CONTEXT *request = SvRequestContext(Request);
    WDF_REQUEST_PARAMETERS parameters;
    WDF_REQUEST_PARAMETERS_INIT(&parameters);
    WdfRequestGetParameters(Request, &parameters);
    request->Offset = (ULONGLONG)parameters.Parameters.Write.DeviceOffset;
    request->Length = (ULONGLONG)Length;
    request->IsWrite = TRUE;
    request->IsPaging = (BOOLEAN)((WdfRequestWdmGetIrp(Request)->Flags & IRP_PAGING_IO) != 0);
    InterlockedIncrement64(&context->Writes);
    /* Invalidate ALL writes, not only IRP_PAGING_IO, before forwarding. This also
     * covers nonpaging/overlapping writes and partial failed lower writes. */
    WdfSpinLockAcquire(context->Lock);
    sv_book_begin_write(&context->Book, request->Offset, request->Length);
    request->WriteTicketValid = (BOOLEAN)sv_book_write_ticket(&context->Book, request->Offset, request->Length, &request->WriteTicket);
    WdfSpinLockRelease(context->Lock);
    SvSendTracked(device, Request);
}

void SvDefault(WDFQUEUE Queue, WDFREQUEST Request)
{
    WDF_REQUEST_SEND_OPTIONS options;
    WDFDEVICE device = WdfIoQueueGetDevice(Queue);
    SV_REQUEST_CONTEXT *request = SvRequestContext(Request);
    UCHAR major = IoGetCurrentIrpStackLocation(WdfRequestWdmGetIrp(Request))->MajorFunction;
    /* TRIM/UNMAP, passthrough SCSI and vendor controls can mutate blocks without
     * an IRP_MJ_WRITE. Until a readonly allowlist is qualified, conservatively
     * hold a global coherence barrier for ALL forwarded storage controls. */
    if (major == IRP_MJ_DEVICE_CONTROL || major == IRP_MJ_INTERNAL_DEVICE_CONTROL || major == IRP_MJ_FLUSH_BUFFERS) {
        SV_DEVICE_CONTEXT *context = SvDeviceContext(device);
        request->IsWrite = TRUE; request->IsPaging = FALSE; request->WriteTicketValid = FALSE;
        request->ReadStampValid = FALSE; request->Offset = 0;
        request->Length = (ULONGLONG)SV_BOOK_BUCKETS * 4096;
        WdfSpinLockAcquire(context->Lock);
        sv_book_begin_write(&context->Book, request->Offset, request->Length);
        WdfSpinLockRelease(context->Lock);
        SvSendTracked(device, Request);
        return;
    }
    WdfRequestFormatRequestUsingCurrentType(Request);
    WDF_REQUEST_SEND_OPTIONS_INIT(&options, WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET);
    if (!WdfRequestSend(Request, WdfDeviceGetIoTarget(device), &options))
        WdfRequestComplete(Request, WdfRequestGetStatus(Request));
}

void SvDeviceControl(WDFQUEUE Queue, WDFREQUEST Request, size_t OutputBufferLength,
                     size_t InputBufferLength, ULONG IoControlCode)
{
    UNREFERENCED_PARAMETER(InputBufferLength);
    /* Discovery only through the disk stack. Mutating broker IOCTLs require
     * the authenticated file/session created by our independent CDO. */
    if (IoControlCode == IOCTL_SVRAM_QUERY || IoControlCode == IOCTL_SVRAM_OWNER_INFO) {
        SvDeviceControlForDevice(WdfIoQueueGetDevice(Queue),Request,OutputBufferLength,IoControlCode);
    } else SvDefault(Queue,Request);
}
void SvDeviceControlForDevice(WDFDEVICE Device, WDFREQUEST Request, size_t OutputBufferLength, ULONG IoControlCode)
{
    SV_QUERY *query;
    NTSTATUS status;
    SV_DEVICE_CONTEXT *context;
    if (SvCacheControl(Device, Request, OutputBufferLength, IoControlCode)) return;
    if (IoControlCode == IOCTL_SVRAM_OWNER_INFO) {
        SV_OWNER_INFO *info;
        PIRP irp=WdfRequestWdmGetIrp(Request);
        status=WdfRequestRetrieveOutputBuffer(Request,sizeof(*info),(PVOID *)&info,NULL);
        if (!NT_SUCCESS(status)) { WdfRequestComplete(Request,status); return; }
        RtlZeroMemory(info,sizeof(*info)); info->Size=sizeof(*info); info->Version=3;
        info->WdfFilePresent=(ULONG)(WdfRequestGetFileObject(Request)!=NULL);
        info->StackFilePresent=(ULONG)(IoGetCurrentIrpStackLocation(irp)->FileObject!=NULL);
        info->SidebandFilePresent=(ULONG)(SvRequestContext(Request)->CallerSession!=NULL);
        info->RequestorPid=IoGetRequestorProcessId(irp);
        info->ControlId=SvDeviceContext(Device)->ControlId;
        /* Presence booleans only. Never expose kernel object addresses. */
        WdfRequestCompleteWithInformation(Request,STATUS_SUCCESS,sizeof(*info)); return;
    }
    if (IoControlCode != IOCTL_SVRAM_QUERY) { WdfRequestComplete(Request,STATUS_INVALID_DEVICE_REQUEST); return; }
    if (OutputBufferLength < sizeof(SV_QUERY)) {
        WdfRequestComplete(Request, STATUS_BUFFER_TOO_SMALL); return;
    }
    status = WdfRequestRetrieveOutputBuffer(Request, sizeof(SV_QUERY), (PVOID *)&query, NULL);
    if (!NT_SUCCESS(status)) { WdfRequestComplete(Request, status); return; }
    context = SvDeviceContext(Device);
    RtlZeroMemory(query, sizeof(*query));
    query->Size = sizeof(*query); query->Version = 4; query->Mode = SV_MODE_BYPASS_ONLY;
    query->ProtocolVersion=SV_PROTOCOL_VERSION; query->MaximumTransferBytes=SV_RANGE_BYTES;
    if (SV_ENABLE_EXPERIMENTAL_CACHE) query->Mode = context->CacheEnabled ? 2 : 1;
    query->PagingPaths = InterlockedCompareExchange(&context->PagingPaths, 0, 0);
    query->Reads = (ULONGLONG)InterlockedCompareExchange64(&context->Reads, 0, 0);
    query->Writes = (ULONGLONG)InterlockedCompareExchange64(&context->Writes, 0, 0);
    query->PagingReads = (ULONGLONG)InterlockedCompareExchange64(&context->PagingReads, 0, 0);
    query->Candidates = (ULONGLONG)InterlockedCompareExchange64(&context->Candidates, 0, 0);
    query->CacheHits = (ULONGLONG)InterlockedCompareExchange64(&context->CacheHits, 0, 0);
    query->IssuedReads = (ULONGLONG)InterlockedCompareExchange64(&context->IssuedReads, 0, 0);
    query->DeadlineFallbacks = (ULONGLONG)InterlockedCompareExchange64(&context->DeadlineFallbacks, 0, 0);
    query->ReplyFallbacks = (ULONGLONG)InterlockedCompareExchange64(&context->ReplyFallbacks, 0, 0);
    query->CancelledReads = (ULONGLONG)InterlockedCompareExchange64(&context->CancelledReads, 0, 0);
    query->LateReplies = (ULONGLONG)InterlockedCompareExchange64(&context->LateReplies, 0, 0);
    query->OfferedFills = (ULONGLONG)InterlockedCompareExchange64(&context->OfferedFills, 0, 0);
    query->CacheLeases = InterlockedCompareExchange(&context->CacheLeases, 0, 0);
    query->CacheD0Available = (ULONG)InterlockedCompareExchange((volatile LONG *)&context->InD0, 0, 0);
    query->QuiesceCalls = (ULONGLONG)InterlockedCompareExchange64(&context->QuiesceCalls, 0, 0);
    WdfRequestCompleteWithInformation(Request, STATUS_SUCCESS, sizeof(*query));
}
