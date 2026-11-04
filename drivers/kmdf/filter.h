#pragma once
#include <ntifs.h>
#include <wdf.h>
#include "protocol.h"

/* Independent control-session lifetime. Pending parent requests may outlive
 * the child PDO's WDF resources; they hold only this nonpaged token. */
typedef struct _SV_SESSION { volatile LONG References, Closing; } SV_SESSION;
typedef struct _SV_FILE_CONTEXT { SV_SESSION *Session; } SV_FILE_CONTEXT;
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(SV_FILE_CONTEXT,SvFileContext);
void SvSessionReference(SV_SESSION *Session);
void SvSessionRelease(SV_SESSION *Session);

typedef struct _SV_PENDING {
    WDFREQUEST Request;
    SV_REQUEST_STATE State;
    SV_RANGE_STAMP Range;
    BOOLEAN CancelPending;
} SV_PENDING;
typedef struct _SV_DEVICE_CONTEXT {
    WDFSPINLOCK Lock;
    SV_BOOK Book;
    volatile LONG64 Reads, Writes, PagingReads, Candidates, CacheHits;
    volatile LONG64 IssuedReads, DeadlineFallbacks, ReplyFallbacks, CancelledReads, LateReplies, OfferedFills;
    volatile LONG PagingPaths, CacheEnabled, BrokerWaiting;
    ULONG BrokerProcessId;
    SV_SESSION *BrokerSession;
    ULONG Disabling;
    ULONG InD0;
    ULONG CacheOperational;
    LONG CacheLeases;
    KEVENT CacheDrained;
    volatile LONG64 QuiesceCalls;
    ULONGLONG BrokerEpoch;
    ULONGLONG NextSequence;
    WDFQUEUE BrokerQueue;
    WDFDEVICE ControlDevice;
    ULONGLONG ControlId;
    WDFQUEUE FallbackQueue;
    WDFTIMER DeadlineTimer;
    SV_PENDING Pending[SV_PENDING_LIMIT];
} SV_DEVICE_CONTEXT;
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(SV_DEVICE_CONTEXT, SvDeviceContext);
typedef struct _SV_REQUEST_CONTEXT {
    BOOLEAN IsWrite, IsPaging, ReadStampValid, WriteTicketValid;
    ULONGLONG Offset, Length;
    SV_RANGE_STAMP ReadRange;
    SV_WRITE_TICKET WriteTicket;
    ULONGLONG BrokerEpoch;
    SV_SESSION *CallerSession;
    WDFDEVICE ControlTarget;
} SV_REQUEST_CONTEXT;
WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(SV_REQUEST_CONTEXT, SvRequestContext);

void SvSendTracked(WDFDEVICE Device, WDFREQUEST Request);
NTSTATUS SvCacheInitialize(WDFDEVICE Device);
BOOLEAN SvCacheTryRead(WDFDEVICE Device, WDFREQUEST Request);
void SvCacheOffer(WDFDEVICE Device, WDFREQUEST Request, const SV_STAMP *WriteStamp);
BOOLEAN SvCacheControl(WDFDEVICE Device, WDFREQUEST Request, size_t OutputLength, ULONG Code);
void SvCachePowerBoundary(WDFDEVICE Device, BOOLEAN InD0);
void SvCacheQuiesce(WDFDEVICE Device, BOOLEAN Purge);
void SvCacheResume(WDFDEVICE Device);
EVT_WDF_FILE_CLEANUP SvBrokerFileCleanup;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL SvDeviceControl;
void SvDeviceControlForDevice(WDFDEVICE Device, WDFREQUEST Request, size_t OutputLength, ULONG Code);
NTSTATUS SvCreateControlDevice(WDFDEVICE Parent);
void SvDeleteControlDevice(WDFDEVICE Parent);
void SvRetireControlDevice(WDFDEVICE Parent);
WDFDEVICE SvControlTarget(WDFDEVICE Control);
BOOLEAN SvControlEnter(WDFDEVICE Control);
void SvControlLeave(WDFDEVICE Control);
EVT_WDF_IO_QUEUE_IO_CANCELED_ON_QUEUE SvBrokerGetCancelled;
