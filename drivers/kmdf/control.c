/* Copyright (c) Microsoft Corporation. All rights reserved.
 * Modifications Copyright (c) 2026 SwappableVRAM contributors.
 * Microsoft Public License (MS-PL), third_party/driver-samples/LICENSE-MS-PL.txt.
 * Independent sideband control device, following the KMDF Toaster pattern.
 * One private endpoint per filter instance; the disk GUID is discovery only. */
#include "filter.h"
#include <ntddk.h>
#include <ntstrsafe.h>
typedef struct _SV_CONTROL_CONTEXT {
    WDFDEVICE Target;
    WDFSPINLOCK Lock;
    WDFQUEUE DispatchQueue;
    KEVENT Drained;
    LONG Operations;
    BOOLEAN Available;
} SV_CONTROL_CONTEXT;
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(SV_CONTROL_CONTEXT,SvControlContext);
EVT_WDF_DEVICE_FILE_CREATE SvControlCreate;
EVT_WDF_FILE_CLOSE SvControlClose;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL SvControlIoctl;
EVT_WDF_OBJECT_CONTEXT_CLEANUP SvControlRequestCleanup;

WDFDEVICE SvControlTarget(WDFDEVICE Control) { return SvControlContext(Control)->Target; }
BOOLEAN SvControlEnter(WDFDEVICE Control)
{
    SV_CONTROL_CONTEXT *context=SvControlContext(Control);
    BOOLEAN entered=FALSE;
    WdfSpinLockAcquire(context->Lock);
    if (context->Available) {
        if (context->Operations++ == 0) KeClearEvent(&context->Drained);
        entered=TRUE;
    }
    WdfSpinLockRelease(context->Lock);
    return entered;
}
void SvControlLeave(WDFDEVICE Control)
{
    SV_CONTROL_CONTEXT *context=SvControlContext(Control);
    WdfSpinLockAcquire(context->Lock);
    NT_ASSERT(context->Operations > 0);
    if (--context->Operations == 0) KeSetEvent(&context->Drained,IO_NO_INCREMENT,FALSE);
    WdfSpinLockRelease(context->Lock);
}
void SvSessionReference(SV_SESSION *Session) { InterlockedIncrement(&Session->References); }
void SvSessionRelease(SV_SESSION *Session)
{
    if (Session != NULL && InterlockedDecrement(&Session->References) == 0) ExFreePoolWithTag(Session,'sRvS');
}
void SvControlCreate(WDFDEVICE Device, WDFREQUEST Request, WDFFILEOBJECT FileObject)
{
    SV_SESSION *session;
    if (!SvControlEnter(Device)) { WdfRequestComplete(Request,STATUS_DEVICE_REMOVED); return; }
    if (WdfFileObjectGetFileName(FileObject)->Length != 0) {
        WdfRequestComplete(Request,STATUS_OBJECT_NAME_INVALID); SvControlLeave(Device); return;
    }
    session=ExAllocatePool2(POOL_FLAG_NON_PAGED,sizeof(*session),'sRvS');
    if (session == NULL) { WdfRequestComplete(Request,STATUS_INSUFFICIENT_RESOURCES); SvControlLeave(Device); return; }
    session->References=1; session->Closing=0;
    SvFileContext(FileObject)->Session=session;
    WdfRequestComplete(Request,STATUS_SUCCESS);
    SvControlLeave(Device);
}
void SvControlClose(WDFFILEOBJECT FileObject)
{
    SV_SESSION *session=SvFileContext(FileObject)->Session;
    SvFileContext(FileObject)->Session=NULL;
    SvSessionRelease(session);
}
void SvControlRequestCleanup(WDFOBJECT Object)
{
    SV_REQUEST_CONTEXT *context=SvRequestContext((WDFREQUEST)Object);
    SV_SESSION *session=context->CallerSession;
    context->CallerSession=NULL;
    /* No parent/child resources here: a late cleanup only drops a token. */
    SvSessionRelease(session);
}
void SvControlIoctl(WDFQUEUE Queue, WDFREQUEST Request, size_t OutputLength, size_t InputLength, ULONG Code)
{
    WDFDEVICE control=WdfIoQueueGetDevice(Queue);
    WDFFILEOBJECT file;
    SV_SESSION *session;
    SV_REQUEST_CONTEXT *request=SvRequestContext(Request);
    UNREFERENCED_PARAMETER(InputLength);
    if (!SvControlEnter(control)) { WdfRequestComplete(Request,STATUS_DEVICE_REMOVED); return; }
    file=WdfRequestGetFileObject(Request);
    session=file == NULL ? NULL : SvFileContext(file)->Session;
    if (session == NULL || request->CallerSession != NULL || InterlockedCompareExchange(&session->Closing,0,0)) {
        WdfRequestComplete(Request,STATUS_FILE_CLOSED);
    } else {
        SvSessionReference(session); request->CallerSession=session;
        request->ControlTarget=SvControlTarget(control);
        SvDeviceControlForDevice(request->ControlTarget,Request,OutputLength,Code);
    }
    SvControlLeave(control);
}
NTSTATUS SvCreateControlDevice(WDFDEVICE Parent)
{
    DECLARE_CONST_UNICODE_STRING(security,L"D:P(A;;GA;;;SY)(A;;GA;;;BA)");
    WCHAR nameBuffer[96],linkBuffer[96];
    UNICODE_STRING name,link;
    LUID id;
    PWDFDEVICE_INIT init=NULL;
    WDFDEVICE control=NULL;
    WDF_OBJECT_ATTRIBUTES attributes,requests,filesAttributes;
    WDF_FILEOBJECT_CONFIG files;
    WDF_IO_QUEUE_CONFIG queue;
    SV_CONTROL_CONTEXT *context;
    SV_DEVICE_CONTEXT *target=SvDeviceContext(Parent);
    NTSTATUS status=ZwAllocateLocallyUniqueId(&id);
    if (!NT_SUCCESS(status)) return status;
    target->ControlId=((ULONGLONG)(ULONG)id.HighPart<<32)|id.LowPart;
    status=RtlStringCchPrintfW(nameBuffer,RTL_NUMBER_OF(nameBuffer),L"\\Device\\SwappableVRAM-%016I64X",target->ControlId);
    if (!NT_SUCCESS(status)) return status;
    status=RtlStringCchPrintfW(linkBuffer,RTL_NUMBER_OF(linkBuffer),L"\\DosDevices\\Global\\SwappableVRAM-%016I64X",target->ControlId);
    if (!NT_SUCCESS(status)) return status;
    RtlInitUnicodeString(&name,nameBuffer); RtlInitUnicodeString(&link,linkBuffer);
    init=WdfControlDeviceInitAllocate(WdfDeviceGetDriver(Parent),&security);
    if (init == NULL) return STATUS_INSUFFICIENT_RESOURCES;
    status=WdfDeviceInitAssignName(init,&name); if (!NT_SUCCESS(status)) goto Fail;
    WdfDeviceInitSetDeviceType(init,FILE_DEVICE_UNKNOWN);
    WdfDeviceInitSetCharacteristics(init,FILE_DEVICE_SECURE_OPEN,TRUE);
    WDF_FILEOBJECT_CONFIG_INIT(&files,SvControlCreate,SvControlClose,SvBrokerFileCleanup);
    files.FileObjectClass=WdfFileObjectWdfCannotUseFsContexts;
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&filesAttributes,SV_FILE_CONTEXT);
    WdfDeviceInitSetFileObjectConfig(init,&files,&filesAttributes);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&requests,SV_REQUEST_CONTEXT);
    requests.EvtCleanupCallback=SvControlRequestCleanup;
    WdfDeviceInitSetRequestAttributes(init,&requests);
    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes,SV_CONTROL_CONTEXT);
    attributes.ExecutionLevel=WdfExecutionLevelPassive;
    attributes.SynchronizationScope=WdfSynchronizationScopeNone;
    status=WdfDeviceCreate(&init,&attributes,&control); if (!NT_SUCCESS(status)) goto Fail;
    init=NULL;
    context=SvControlContext(control);
    WdfObjectReference(Parent); context->Target=Parent;
    KeInitializeEvent(&context->Drained,NotificationEvent,TRUE);
    WDF_OBJECT_ATTRIBUTES_INIT(&attributes); attributes.ParentObject=control;
    status=WdfSpinLockCreate(&attributes,&context->Lock); if (!NT_SUCCESS(status)) goto Fail;
    WDF_IO_QUEUE_CONFIG_INIT(&queue,WdfIoQueueDispatchManual);
    queue.PowerManaged=WdfFalse; queue.EvtIoCanceledOnQueue=SvBrokerGetCancelled;
    status=WdfIoQueueCreate(control,&queue,WDF_NO_OBJECT_ATTRIBUTES,&target->BrokerQueue); if (!NT_SUCCESS(status)) goto Fail;
    WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queue,WdfIoQueueDispatchParallel);
    queue.PowerManaged=WdfFalse; queue.EvtIoDeviceControl=SvControlIoctl;
    status=WdfIoQueueCreate(control,&queue,WDF_NO_OBJECT_ATTRIBUTES,&context->DispatchQueue); if (!NT_SUCCESS(status)) goto Fail;
    status=WdfDeviceCreateSymbolicLink(control,&link); if (!NT_SUCCESS(status)) goto Fail;
    target->ControlDevice=control; context->Available=TRUE;
    WdfControlFinishInitializing(control);
    return STATUS_SUCCESS;
Fail:
    if (init != NULL) WdfDeviceInitFree(init);
    target->BrokerQueue=NULL;
    if (control != NULL) {
        context=SvControlContext(control);
        if (context->Target != NULL) { context->Target=NULL; WdfObjectDereference(Parent); }
        WdfObjectDelete(control);
    }
    return status;
}
void SvRetireControlDevice(WDFDEVICE Parent)
{
    SV_DEVICE_CONTEXT *target=SvDeviceContext(Parent);
    WDFDEVICE control=target->ControlDevice;
    SV_CONTROL_CONTEXT *context;
    if (control == NULL) return;
    context=SvControlContext(control);
    WdfSpinLockAcquire(context->Lock); context->Available=FALSE; WdfSpinLockRelease(context->Lock);
    /* FDO quiesce has closed the cache gate and drained cache CPU leases.
     * These are independent CDO queues; no cross-device queue forwarding. */
    WdfIoQueuePurgeSynchronously(context->DispatchQueue);
    WdfIoQueuePurgeSynchronously(target->BrokerQueue);
    (void)KeWaitForSingleObject(&context->Drained,Executive,KernelMode,FALSE,NULL);
}
void SvDeleteControlDevice(WDFDEVICE Parent)
{
    SV_DEVICE_CONTEXT *target=SvDeviceContext(Parent);
    WDFDEVICE control=target->ControlDevice;
    if (control == NULL) return;
    SvRetireControlDevice(Parent);
    target->ControlDevice=NULL; target->BrokerQueue=NULL;
    /* All target operations/cancel callbacks are drained before the FDO's
     * cleanup returns. A late CDO close touches only its independent token. */
    SvControlContext(control)->Target=NULL;
    WdfObjectDereference(Parent);
    WdfObjectDelete(control);
}
