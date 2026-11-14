/*
 * mpegts: MPEG transport stream analyser and demultiplexer.
 *
 * Stable C ABI. Rules that hold for every function:
 *
 *  - Functions return an mpegts_status (0 on success, negative on error)
 *    unless documented otherwise. They never abort or unwind into C; an
 *    internal panic is reported as MPEGTS_ERR_PANIC and the demuxer is then
 *    unusable (MPEGTS_ERR_POISONED) and must be freed.
 *  - Every pointer argument is checked for NULL.
 *  - Structs that cross the boundary start with `struct_size`. Set it to
 *    sizeof(the struct) before passing it in. The library reads or writes
 *    only that many bytes, so newer libraries work with older headers and
 *    the other way round. On output, struct_size is set to the number of
 *    bytes written.
 *  - A demuxer is not thread-safe: use each one from one thread at a time
 *    (it may move between threads). Separate demuxers are independent. A
 *    report never changes, so any number of threads may read one at once.
 *  - Pointers returned into a demuxer or report stay valid until the call
 *    documented for them, or until the object is freed. Strings returned
 *    by the library are static.
 *
 * Compatibility: within one MPEGTS_ABI_VERSION, functions, constants and
 * struct fields are only ever added (fields at the end), never changed.
 * The shared library's soname carries the ABI version (libmpegts.so.1).
 * To detect a mismatched library at run time, compare
 * mpegts_abi_version() with MPEGTS_ABI_VERSION.
 */
#ifndef MPEGTS_H
#define MPEGTS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MPEGTS_ABI_VERSION 1

/* Version of the library this header came with. */
#define MPEGTS_VERSION_MAJOR 0
#define MPEGTS_VERSION_MINOR 1
#define MPEGTS_VERSION_PATCH 0
#define MPEGTS_VERSION_STRING "0.1.0"

/* ------------------------------------------------------------ status */

typedef int32_t mpegts_status;
#define MPEGTS_OK 0
#define MPEGTS_EMPTY 1            /* nothing to return (no output waiting) */
#define MPEGTS_ERR_NULL (-1)      /* a required pointer was NULL */
#define MPEGTS_ERR_INVALID (-2)   /* an argument or struct field is invalid */
#define MPEGTS_ERR_RANGE (-3)     /* an index is out of range */
#define MPEGTS_ERR_NOT_FOUND (-4) /* no such subscription */
#define MPEGTS_ERR_PANIC (-5)     /* internal error; the demuxer is poisoned */
#define MPEGTS_ERR_POISONED (-6)  /* the demuxer failed earlier; free it */

/* ------------------------------------------------------------ codes */

/* Stream kinds. */
#define MPEGTS_KIND_UNKNOWN 0
#define MPEGTS_KIND_VIDEO 1
#define MPEGTS_KIND_AUDIO 2
#define MPEGTS_KIND_METADATA 3
#define MPEGTS_KIND_SUBTITLE 4
#define MPEGTS_KIND_DATA 5

/* How a stream was identified. */
#define MPEGTS_BASIS_STREAM_TYPE 1
#define MPEGTS_BASIS_DESCRIPTOR 2
#define MPEGTS_BASIS_PAYLOAD 3

/* Codecs: video 0x1xx, audio 0x2xx, metadata 0x3xx, subtitles 0x4xx,
 * data 0x5xx. Codes are never reused. */
#define MPEGTS_CODEC_MPEG1_VIDEO 0x101
#define MPEGTS_CODEC_MPEG2_VIDEO 0x102
#define MPEGTS_CODEC_MPEG4_VISUAL 0x103
#define MPEGTS_CODEC_H264 0x104
#define MPEGTS_CODEC_H265 0x105
#define MPEGTS_CODEC_H266 0x106
#define MPEGTS_CODEC_AV1 0x107
#define MPEGTS_CODEC_VC1 0x108
#define MPEGTS_CODEC_JPEG2000 0x109
#define MPEGTS_CODEC_JPEG_XS 0x10A
#define MPEGTS_CODEC_EVC 0x10B
#define MPEGTS_CODEC_LCEVC 0x10C
#define MPEGTS_CODEC_MPEG1_AUDIO 0x201
#define MPEGTS_CODEC_MPEG2_AUDIO 0x202
#define MPEGTS_CODEC_AAC_ADTS 0x203
#define MPEGTS_CODEC_AAC_LATM 0x204
#define MPEGTS_CODEC_MPEG4_AUDIO 0x205
#define MPEGTS_CODEC_AAC 0x206
#define MPEGTS_CODEC_AC3 0x207
#define MPEGTS_CODEC_EAC3 0x208
#define MPEGTS_CODEC_AC4 0x209
#define MPEGTS_CODEC_DTS 0x20A
#define MPEGTS_CODEC_TRUE_HD 0x20B
#define MPEGTS_CODEC_LPCM 0x20C
#define MPEGTS_CODEC_OPUS 0x20D
#define MPEGTS_CODEC_SMPTE302M 0x20E
#define MPEGTS_CODEC_MPEG_H3D_AUDIO 0x20F
#define MPEGTS_CODEC_KLV_ASYNC 0x301
#define MPEGTS_CODEC_KLV_SYNC 0x302
#define MPEGTS_CODEC_ID3 0x303
#define MPEGTS_CODEC_SCTE35 0x304
#define MPEGTS_CODEC_TEMI 0x305
#define MPEGTS_CODEC_METADATA 0x306
#define MPEGTS_CODEC_DVB_SUBTITLE 0x401
#define MPEGTS_CODEC_TELETEXT 0x402
#define MPEGTS_CODEC_PGS_SUBTITLE 0x403
#define MPEGTS_CODEC_TTML_SUBTITLE 0x404
#define MPEGTS_CODEC_DSMCC 0x501
#define MPEGTS_CODEC_SMPTE2038 0x502
#define MPEGTS_CODEC_PRIVATE_SECTIONS 0x503
#define MPEGTS_CODEC_PRIVATE_DATA 0x504
#define MPEGTS_CODEC_UNKNOWN 0x000

/* What a subscription follows (mpegts_subscription.select). */
#define MPEGTS_SELECT_PID 1     /* value = PID */
#define MPEGTS_SELECT_PROGRAM 2 /* value = program_number */
#define MPEGTS_SELECT_CODEC 3   /* value = MPEGTS_CODEC_* */
#define MPEGTS_SELECT_KIND 4    /* value = MPEGTS_KIND_* */
#define MPEGTS_SELECT_PSI 5     /* PAT, CAT, PMTs, NIT, SI PIDs; value ignored */
#define MPEGTS_SELECT_ALL 6     /* every PID; value ignored */

/* What a subscription delivers (mpegts_subscription.unit). */
#define MPEGTS_UNIT_PACKETS 1  /* raw 188-byte TS packets */
#define MPEGTS_UNIT_PES 2      /* reassembled PES packets */
#define MPEGTS_UNIT_SECTIONS 3 /* complete sections */

/* Output queue overflow policy (mpegts_config.overflow). */
#define MPEGTS_OVERFLOW_DROP_NEWEST 0
#define MPEGTS_OVERFLOW_DROP_OLDEST 1

/* ------------------------------------------------------------ types */

typedef struct mpegts_demuxer mpegts_demuxer; /* opaque */
typedef struct mpegts_report mpegts_report;   /* opaque snapshot */

/* Limits and buffer sizes; see the Rust `Config` docs. Fill with
 * mpegts_config_default, then change fields. */
typedef struct mpegts_config {
    uint32_t struct_size;
    uint32_t sync_confirmations; /* 1..8, default 2 */
    uint32_t probe_pes;          /* PES packets inspected to identify ambiguous streams, default 4 */
    uint32_t overflow;           /* MPEGTS_OVERFLOW_*, default DROP_NEWEST */
    uint64_t max_streams;        /* default 4096 */
    uint64_t history_packets;    /* recent packets kept for replay, default 0 */
    uint64_t queue_max_items;    /* default 65536 */
    uint64_t queue_max_bytes;    /* default 64 MiB */
    uint64_t max_pes_bytes;      /* default 8 MiB */
    uint64_t max_reassembly_bytes; /* default 64 MiB */
} mpegts_config;

typedef struct mpegts_subscription {
    uint32_t struct_size;
    uint32_t select; /* MPEGTS_SELECT_* */
    uint32_t value;  /* see MPEGTS_SELECT_* */
    uint32_t unit;   /* MPEGTS_UNIT_* */
    uint32_t replay; /* non-zero: first deliver matching data from the history */
    uint32_t strip_metadata_au; /* non-zero: PES from metadata streams (stream_id
                                 * 0xFC, e.g. sync KLV) without their 5-byte
                                 * access unit cell headers, so the payload is
                                 * just the metadata. Default 0: as carried. */
} mpegts_subscription;

/* mpegts_output.flags */
#define MPEGTS_OUTPUT_HAS_STREAM_ID 0x01
#define MPEGTS_OUTPUT_HAS_TABLE_ID 0x02
#define MPEGTS_OUTPUT_HAS_PTS 0x04
#define MPEGTS_OUTPUT_HAS_DTS 0x08
#define MPEGTS_OUTPUT_CRC_OK 0x10
#define MPEGTS_OUTPUT_CRC_BAD 0x20
#define MPEGTS_OUTPUT_DAMAGED 0x40   /* data missing inside this unit */
#define MPEGTS_OUTPUT_TRUNCATED 0x80 /* cut short at a size limit */

typedef struct mpegts_output {
    uint32_t struct_size;
    uint32_t subscription;
    uint32_t unit;  /* MPEGTS_UNIT_* */
    uint32_t flags; /* MPEGTS_OUTPUT_* */
    uint16_t pid;
    uint8_t stream_id; /* PES stream_id, with HAS_STREAM_ID */
    uint8_t table_id;  /* section table_id, with HAS_TABLE_ID */
    uint32_t reserved;
    uint64_t packet; /* feed packet number of the last packet that contributed */
    uint64_t pts;    /* 90 kHz, with HAS_PTS */
    uint64_t dts;    /* 90 kHz, with HAS_DTS */
    const uint8_t *data;     /* packet, whole PES packet, or whole section */
    uint64_t data_len;
    uint64_t payload_offset; /* PES: start of the elementary stream bytes in data */
} mpegts_output;

typedef struct mpegts_output_stats {
    uint32_t struct_size;
    uint32_t subscriptions;
    uint64_t queued;
    uint64_t dropped;
    uint64_t truncated;
    uint64_t damaged;
    uint64_t pending;
    uint64_t pending_bytes;
    uint64_t reassembly_bytes;
} mpegts_output_stats;

/* mpegts_report_summary.flags */
#define MPEGTS_SUMMARY_HAS_TSID 0x01
#define MPEGTS_SUMMARY_HAS_PAT_VERSION 0x02
#define MPEGTS_SUMMARY_HAS_NETWORK_PID 0x04
#define MPEGTS_SUMMARY_HAS_PACKET_SIZE 0x08
#define MPEGTS_SUMMARY_HAS_DURATION 0x10
#define MPEGTS_SUMMARY_HAS_BITRATE 0x20

typedef struct mpegts_report_summary {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_SUMMARY_* */
    uint64_t revision;
    uint16_t transport_stream_id;
    uint16_t network_pid;
    uint8_t pat_version;
    uint8_t reserved[3];
    uint32_t packet_size; /* 188, 192 or 204 */
    uint32_t program_count;
    uint32_t pid_count;
    uint32_t ca_system_count;
    uint64_t packets;
    uint64_t bytes_skipped;
    uint64_t sync_losses;
    uint64_t crc_errors;
    uint64_t streams_dropped;
    uint64_t duration_ns;
    uint64_t bitrate; /* bits per second */
} mpegts_report_summary;

/* mpegts_program_info.flags */
#define MPEGTS_PROGRAM_HAS_VERSION 0x01
#define MPEGTS_PROGRAM_HAS_PCR_PID 0x02
#define MPEGTS_PROGRAM_HAS_REGISTRATION 0x04
#define MPEGTS_PROGRAM_HAS_FIRST_PCR 0x08
#define MPEGTS_PROGRAM_HAS_LAST_PCR 0x10
#define MPEGTS_PROGRAM_HAS_PCR_SPAN 0x20

typedef struct mpegts_program_info {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_PROGRAM_* */
    uint16_t number;
    uint16_t pmt_pid;
    uint16_t pcr_pid;
    uint8_t version;
    uint8_t reserved;
    uint8_t registration[4];
    uint32_t stream_count;
    uint32_t descriptor_count;
    uint32_t reserved2;
    uint64_t first_pcr; /* 27 MHz */
    uint64_t last_pcr;
    uint64_t pcr_span;
} mpegts_program_info;

/* mpegts_stream_info.flags */
#define MPEGTS_STREAM_HAS_LANGUAGE 0x001
#define MPEGTS_STREAM_HAS_REGISTRATION 0x002
#define MPEGTS_STREAM_HAS_METADATA_FORMAT 0x004
#define MPEGTS_STREAM_HAS_PES_STREAM_ID 0x008
#define MPEGTS_STREAM_HAS_FIRST_PTS 0x010
#define MPEGTS_STREAM_HAS_LAST_PTS 0x020
#define MPEGTS_STREAM_HAS_LAST_DTS 0x040
#define MPEGTS_STREAM_HAS_PTS_SPAN 0x080
#define MPEGTS_STREAM_HAS_BITRATE 0x100
#define MPEGTS_STREAM_HAS_LAST_PACKET 0x200
#define MPEGTS_STREAM_SCRAMBLED 0x400

typedef struct mpegts_stream_info {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_STREAM_* */
    uint16_t program;
    uint16_t pid;
    uint8_t stream_type;
    uint8_t pes_stream_id;
    uint16_t reserved;
    uint32_t codec; /* MPEGTS_CODEC_* */
    uint32_t kind;  /* MPEGTS_KIND_* */
    uint32_t basis; /* MPEGTS_BASIS_* */
    char language[4]; /* NUL-terminated ISO 639-2 code */
    uint8_t registration[4];
    uint8_t metadata_format[4];
    uint32_t descriptor_count;
    uint32_t reserved2;
    uint64_t first_pts; /* 90 kHz */
    uint64_t last_pts;
    uint64_t last_dts;
    uint64_t pts_span;
    uint64_t packets;
    uint64_t cc_errors;
    uint64_t scrambled_packets;
    uint64_t last_packet;
    uint64_t bitrate; /* bits per second */
} mpegts_stream_info;

typedef struct mpegts_pid_info {
    uint32_t struct_size;
    uint16_t pid;
    uint16_t reserved;
    uint64_t packets;
    uint64_t cc_errors;
    uint64_t transport_errors;
    uint64_t scrambled_packets;
    uint64_t last_packet;
} mpegts_pid_info;

/* ------------------------------------------------------------ library */

uint32_t mpegts_abi_version(void);
const char *mpegts_version(void);                 /* e.g. "0.1.0" */
const char *mpegts_status_string(mpegts_status status);
const char *mpegts_codec_name(uint32_t codec);     /* NULL for unknown codes */
uint32_t mpegts_codec_kind(uint32_t codec);        /* MPEGTS_KIND_* */
const char *mpegts_kind_name(uint32_t kind);       /* NULL for unknown codes */

/* ------------------------------------------------------------ demuxer */

mpegts_status mpegts_config_default(mpegts_config *config);
/* MPEGTS_OK if mpegts_demuxer_new would accept `config`. Otherwise returns
 * MPEGTS_ERR_INVALID and, unless `message` is NULL, writes the reason as a
 * NUL-terminated string of at most message_len bytes (256 is plenty). */
mpegts_status mpegts_config_check(const mpegts_config *config, char *message, size_t message_len);

/* `config` may be NULL for defaults. On error *out is set to NULL. */
mpegts_status mpegts_demuxer_new(const mpegts_config *config, mpegts_demuxer **out);
void mpegts_demuxer_free(mpegts_demuxer *demuxer); /* NULL is ignored */

/* Feed bytes, in chunks of any size. `data` may be NULL when len is 0. */
mpegts_status mpegts_demuxer_push(mpegts_demuxer *demuxer, const uint8_t *data, size_t len);
/* End of input: flushes the last packets and PES packets in progress. */
mpegts_status mpegts_demuxer_finish(mpegts_demuxer *demuxer);
/* Changes whenever programs or streams change. */
mpegts_status mpegts_demuxer_revision(const mpegts_demuxer *demuxer, uint64_t *out);

mpegts_status mpegts_demuxer_subscribe(mpegts_demuxer *demuxer, const mpegts_subscription *sub, uint32_t *id);
mpegts_status mpegts_demuxer_unsubscribe(mpegts_demuxer *demuxer, uint32_t id);

/* Take the oldest queued output. Returns MPEGTS_EMPTY when none is waiting.
 * out->data stays valid until the next call to this function, or until the
 * demuxer is freed. */
mpegts_status mpegts_demuxer_next_output(mpegts_demuxer *demuxer, mpegts_output *out);
mpegts_status mpegts_demuxer_output_stats(const mpegts_demuxer *demuxer, mpegts_output_stats *out);

/* Packet history (mpegts_config.history_packets), oldest first. *data
 * points at 188 bytes, valid until the next push, finish or free. */
mpegts_status mpegts_demuxer_history_len(const mpegts_demuxer *demuxer, uint64_t *out);
mpegts_status mpegts_demuxer_history_get(const mpegts_demuxer *demuxer, uint64_t index,
                                         uint64_t *packet, const uint8_t **data);

/* ------------------------------------------------------------ reports */

/* Analyse a whole buffer in one call (demuxer_new, push, finish, report).
 * `data` may be NULL when len is 0. Free with mpegts_report_free. */
mpegts_status mpegts_analyze(const uint8_t *data, size_t len, mpegts_report **out);

/* Snapshot of what the feed carries now. Free with mpegts_report_free;
 * it stays valid after the demuxer is freed. */
mpegts_status mpegts_demuxer_report(const mpegts_demuxer *demuxer, mpegts_report **out);
void mpegts_report_free(mpegts_report *report); /* NULL is ignored */

mpegts_status mpegts_report_summary_get(const mpegts_report *report, mpegts_report_summary *out);
mpegts_status mpegts_report_program(const mpegts_report *report, uint32_t program_index,
                                    mpegts_program_info *out);
mpegts_status mpegts_report_stream(const mpegts_report *report, uint32_t program_index,
                                   uint32_t stream_index, mpegts_stream_info *out);
mpegts_status mpegts_report_pid(const mpegts_report *report, uint32_t index, mpegts_pid_info *out);
mpegts_status mpegts_report_ca_system(const mpegts_report *report, uint32_t index,
                                      uint16_t *system_id, uint16_t *pid);
/* Descriptors, verbatim. *data stays valid until the report is freed. */
mpegts_status mpegts_report_program_descriptor(const mpegts_report *report, uint32_t program_index,
                                               uint32_t descriptor_index, uint8_t *tag,
                                               const uint8_t **data, uint64_t *len);
mpegts_status mpegts_report_stream_descriptor(const mpegts_report *report, uint32_t program_index,
                                              uint32_t stream_index, uint32_t descriptor_index,
                                              uint8_t *tag, const uint8_t **data, uint64_t *len);

#ifdef __cplusplus
}
#endif

#endif /* MPEGTS_H */
