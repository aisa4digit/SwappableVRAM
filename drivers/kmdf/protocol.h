#pragma once

/* The Rust book is accessed only while holding the device's WDFSPINLOCK. */
#define SV_BOOK_BUCKETS 4096
typedef struct _SV_BUCKET {
    ULONGLONG Generation;
    ULONG Writers;
    ULONG Reserved;
} SV_BUCKET;
typedef struct _SV_BOOK {
    ULONGLONG VolumeEpoch;
    ULONGLONG NextGeneration;
    ULONGLONG Floor;
    ULONG GlobalWriters;
    ULONG Disabled;
    SV_BUCKET Buckets[SV_BOOK_BUCKETS];
} SV_BOOK;
typedef struct _SV_STAMP {
    ULONGLONG VolumeEpoch;
    ULONGLONG PageIndex;
    ULONGLONG Generation;
} SV_STAMP;

C_ASSERT(sizeof(SV_BUCKET) == 16);
C_ASSERT(sizeof(SV_BOOK) == 65568);
C_ASSERT(sizeof(SV_STAMP) == 24);
#define SV_RANGE_PAGES 16
#define SV_RANGE_BYTES (4096 * SV_RANGE_PAGES)
typedef struct _SV_RANGE_STAMP {
    ULONGLONG VolumeEpoch, PageIndex;
    ULONG Pages, Reserved;
    ULONGLONG Generations[SV_RANGE_PAGES];
} SV_RANGE_STAMP;
C_ASSERT(sizeof(SV_RANGE_STAMP) == 152);

void sv_book_init(SV_BOOK *Book, ULONGLONG VolumeEpoch);
void sv_book_begin_write(SV_BOOK *Book, ULONGLONG Offset, ULONGLONG Length);
void sv_book_end_write(SV_BOOK *Book, ULONGLONG Offset, ULONGLONG Length);
void sv_book_invalidate_all(SV_BOOK *Book);
ULONG sv_book_stamp(const SV_BOOK *Book, ULONGLONG Offset, SV_STAMP *Stamp);
ULONG sv_book_range_stamp(const SV_BOOK *Book, ULONGLONG Offset, ULONGLONG Length, SV_RANGE_STAMP *Stamp);
typedef struct _SV_WRITE_TICKET { ULONGLONG Generation; ULONGLONG Floor; } SV_WRITE_TICKET;
typedef struct _SV_REQUEST_STATE {
    ULONGLONG Sequence;
    ULONGLONG Generation;
    ULONGLONG Deadline;
    ULONG State;
    ULONG Reserved;
} SV_REQUEST_STATE;
C_ASSERT(sizeof(SV_REQUEST_STATE) == 32);
ULONG sv_book_write_ticket(const SV_BOOK *Book, ULONGLONG Offset, ULONGLONG Length, SV_WRITE_TICKET *Ticket);
ULONG sv_book_ticket_matches(const SV_BOOK *Book, ULONGLONG Offset, ULONGLONG Length, const SV_WRITE_TICKET *Ticket);
void sv_request_prepare(SV_REQUEST_STATE *State, ULONGLONG Sequence, ULONGLONG Generation, ULONGLONG Deadline);
ULONG sv_request_claim_hit(SV_REQUEST_STATE *State, ULONGLONG Sequence, ULONGLONG Generation, ULONGLONG Now);
ULONG sv_request_claim_disk(SV_REQUEST_STATE *State);
ULONG sv_request_claim_cancel(SV_REQUEST_STATE *State);

#ifndef SV_ENABLE_EXPERIMENTAL_CACHE
#define SV_ENABLE_EXPERIMENTAL_CACHE 0
#endif
#define SV_PENDING_LIMIT 16
#define SV_FRAME_MAGIC 0x535652414d4c4142ULL
#define SV_OP_READ 1
#define SV_OP_FILL 2
#define SV_STATUS_HIT 2
#define SV_STATUS_MISS 3
#define SV_PROTOCOL_VERSION 3
#define IOCTL_SVRAM_ENABLE CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_SVRAM_GET CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
#define IOCTL_SVRAM_REPLY CTL_CODE(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
typedef struct _SV_ENABLE {
    ULONG Size;
    ULONG Version;
    ULONG Enabled;
    ULONG Reserved;
} SV_ENABLE;
typedef struct _SV_FRAME {
    ULONGLONG Magic;
    ULONG Version;
    ULONG Operation;
    ULONGLONG Sequence;
    ULONGLONG Deadline;
    ULONG Status;
    ULONG Length;
    SV_RANGE_STAMP Range;
    UCHAR Data[SV_RANGE_BYTES];
} SV_FRAME;
#define SV_FRAME_HEADER FIELD_OFFSET(SV_FRAME,Data)
C_ASSERT(SV_FRAME_HEADER == 192);
C_ASSERT(sizeof(SV_FRAME) == 65728);

#define IOCTL_SVRAM_QUERY CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)
#define IOCTL_SVRAM_OWNER_INFO CTL_CODE(FILE_DEVICE_UNKNOWN, 0x904, METHOD_BUFFERED, FILE_READ_ACCESS)
typedef struct _SV_OWNER_INFO {
    ULONG Size, Version, WdfFilePresent, StackFilePresent, SidebandFilePresent, RequestorPid;
    ULONGLONG ControlId;
} SV_OWNER_INFO;
C_ASSERT(sizeof(SV_OWNER_INFO) == 32);
#define SV_MODE_BYPASS_ONLY 0
typedef struct _SV_QUERY {
    ULONG Size;
    ULONG Version;
    ULONG Mode;
    LONG PagingPaths;
    ULONGLONG Reads;
    ULONGLONG Writes;
    ULONGLONG PagingReads;
    ULONGLONG Candidates;
    ULONGLONG CacheHits;
    ULONGLONG IssuedReads;
    ULONGLONG DeadlineFallbacks;
    ULONGLONG ReplyFallbacks;
    ULONGLONG CancelledReads;
    ULONGLONG LateReplies;
    ULONGLONG OfferedFills;
    LONG CacheLeases;
    ULONG CacheD0Available;
    ULONGLONG QuiesceCalls;
    ULONG ProtocolVersion, MaximumTransferBytes;
} SV_QUERY;
C_ASSERT(sizeof(SV_QUERY) == 128);
