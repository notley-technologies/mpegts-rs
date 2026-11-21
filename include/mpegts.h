/*
 * mpegts: MPEG transport stream analyser and demultiplexer.
 *
 * Stable C ABI. Rules that hold for every function:
 *
 *  - Functions return an mpegts_status (0 on success, negative on error)
 *    unless documented otherwise. They never abort or unwind into C; an
 *    internal panic is reported as MPEGTS_ERR_PANIC and the demuxer (or
 *    program tables, PES writer or injector) is then unusable
 *    (MPEGTS_ERR_POISONED) and must be freed.
 *  - Every pointer argument is checked for NULL.
 *  - Structs that cross the boundary start with `struct_size`. Set it to
 *    sizeof(the struct) before passing it in. The library reads or writes
 *    only that many bytes, so newer libraries work with older headers and
 *    the other way round. On output, struct_size is set to the number of
 *    bytes written.
 *  - A demuxer is not thread-safe: use each one from one thread at a time
 *    (it may move between threads). Separate demuxers are independent. A
 *    report never changes, so any number of threads may read one at once.
 *    Program tables, PES writers and injectors follow the demuxer's rule.
 *  - Pointers returned into a demuxer or report stay valid until the call
 *    documented for them, or until the object is freed. Strings returned
 *    by the library are static.
 *  - Functions that write into a caller's buffer take (out, capacity,
 *    written). On MPEGTS_OK and on MPEGTS_ERR_BUFFER, *written is the
 *    number of bytes the output needs; pass NULL and capacity 0 to ask for
 *    it. A buffer that is too small is left untouched and nothing changes
 *    (no continuity counter, sequence number or version moves). `out` may
 *    be NULL only when capacity is 0.
 *  - Functions that produce a stream of unknown length (the injector) hand
 *    it to a caller callback instead, whole 188-byte packets at a time,
 *    before they return. Callbacks must return normally (no longjmp) and
 *    must not call back into the object that called them, except through
 *    the handle they are given.
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
#define MPEGTS_VERSION_MINOR 2
#define MPEGTS_VERSION_PATCH 0
#define MPEGTS_VERSION_STRING "0.2.0"

/* ------------------------------------------------------------ status */

typedef int32_t mpegts_status;
#define MPEGTS_OK 0
#define MPEGTS_EMPTY 1            /* nothing to return (no output waiting) */
#define MPEGTS_ERR_NULL (-1)      /* a required pointer was NULL */
#define MPEGTS_ERR_INVALID (-2)   /* an argument or struct field is invalid */
#define MPEGTS_ERR_RANGE (-3)     /* an index is out of range */
#define MPEGTS_ERR_NOT_FOUND (-4) /* no such subscription, program or stream */
#define MPEGTS_ERR_PANIC (-5)     /* internal error; the object is poisoned */
#define MPEGTS_ERR_POISONED (-6)  /* the object failed earlier; free it */
#define MPEGTS_ERR_BUFFER (-7)    /* output buffer too small; see *written */
#define MPEGTS_ERR_FULL (-8)      /* a queue is full; try again after more input */
#define MPEGTS_ERR_STREAM (-9)    /* the input cannot be processed as configured; the
                                   * object is unusable (see its _error function) */

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
                                 * just the metadata. A payload that is not
                                 * valid cells is left as is and flagged
                                 * DAMAGED. Default 0: as carried. */
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
const char *mpegts_version(void);                 /* e.g. "0.2.0" */
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

/* ------------------------------------------------------------ writing
 *
 * Transport stream write primitives: PAT and PMT, PES packets, metadata
 * access unit cells and the KLV carriage of MISB ST 1402.2. They exist in
 * libraries built with the `mux` feature, as `make` builds them. Output is
 * whole 188-byte packets in the caller's buffer (see the buffer rule at the
 * top). Values written follow docs/standards/DECISIONS.md.
 */

typedef struct mpegts_program_tables mpegts_program_tables; /* opaque */
typedef struct mpegts_pes_writer mpegts_pes_writer;         /* opaque */

/* How a stream's units become PES packets (mpegts_stream_spec.carriage). */
#define MPEGTS_CARRIAGE_PES 1            /* video, audio, private data */
#define MPEGTS_CARRIAGE_METADATA_CELLS 2 /* synchronous KLV: stream_id 0xFC, access
                                          * unit cells, a PTS on every PES */
#define MPEGTS_CARRIAGE_ASYNC_KLV 3      /* SMPTE RP 217: stream_id 0xBD, no PTS */

/* mpegts_stream_spec.flags */
#define MPEGTS_SPEC_SYNC_REGISTRATION 0x01 /* METADATA_CELLS: also write the
                                            * "KLVA" registration_descriptor */

/* A stream's PMT entry and PES format. Fill with one of the
 * mpegts_stream_spec_* functions, then change fields if needed. */
typedef struct mpegts_stream_spec {
    uint32_t struct_size;
    uint32_t carriage;    /* MPEGTS_CARRIAGE_* */
    uint8_t stream_type;  /* PMT stream_type (0x15 sync KLV, 0x06 async KLV) */
    uint8_t stream_id;    /* PES stream_id (0xFC sync KLV, 0xBD async KLV) */
    uint8_t service_id;   /* METADATA_CELLS: metadata_service_id */
    uint8_t reserved;
    uint32_t flags;       /* MPEGTS_SPEC_* */
    /* METADATA_CELLS: metadata_STD_descriptor fields, 22 bits each.
     * Defaults 0x3FFFFF (x 400 bit/s), 0x400 (x 1024 bytes) and 0. */
    uint32_t input_leak_rate;
    uint32_t buffer_size;
    uint32_t output_leak_rate;
} mpegts_stream_spec;

/* Plain PES with no descriptors (add them to the tables separately). */
mpegts_status mpegts_stream_spec_pes(mpegts_stream_spec *spec, uint8_t stream_type, uint8_t stream_id);
/* Synchronous KLV: stream_type 0x15, stream_id 0xFC, metadata_descriptor
 * 26 09 01 00 FF 4B 4C 56 41 <service_id> 0F and metadata_STD_descriptor
 * 27 09 FF FF FF C0 04 00 C0 00 00. */
mpegts_status mpegts_stream_spec_klv_sync(mpegts_stream_spec *spec, uint8_t service_id);
/* Asynchronous KLV: stream_type 0x06, stream_id 0xBD, registration "KLVA". */
mpegts_status mpegts_stream_spec_klv_async(mpegts_stream_spec *spec);

/* Program tables: a PAT and its PMTs, kept consistent. Every change is
 * checked whole (PID ranges and conflicts, sections of at most 1024 bytes)
 * and either applied, with the version_number of each changed table
 * incremented, or refused with MPEGTS_ERR_INVALID (MPEGTS_ERR_NOT_FOUND for
 * an unknown program or stream) and nothing changed. A PID of 0x1FFF means
 * "none" for pcr_pid and the network PID. */
mpegts_status mpegts_program_tables_new(uint16_t transport_stream_id, mpegts_program_tables **out);
/* The tables a report describes: versions, programs, streams and
 * descriptors verbatim. MPEGTS_ERR_INVALID until the PAT and every PMT have
 * been received. A later change bumps each reported version by one. */
mpegts_status mpegts_program_tables_from_report(const mpegts_report *report, mpegts_program_tables **out);
void mpegts_program_tables_free(mpegts_program_tables *tables); /* NULL is ignored */
/* Why the last call on `tables` failed (empty after a success), as a
 * NUL-terminated string of at most message_len bytes. */
mpegts_status mpegts_program_tables_error(const mpegts_program_tables *tables, char *message, size_t message_len);
mpegts_status mpegts_program_tables_add_program(mpegts_program_tables *tables, uint16_t number, uint16_t pmt_pid,
                                                uint16_t pcr_pid);
mpegts_status mpegts_program_tables_remove_program(mpegts_program_tables *tables, uint16_t number);
mpegts_status mpegts_program_tables_set_pcr_pid(mpegts_program_tables *tables, uint16_t number, uint16_t pcr_pid);
/* The PAT's program 0 entry (network information table). */
mpegts_status mpegts_program_tables_set_network_pid(mpegts_program_tables *tables, uint16_t pid);
/* Descriptors are given as tag and body (without the length byte, at most
 * 255 bytes). `data` may be NULL when len is 0. */
mpegts_status mpegts_program_tables_add_program_descriptor(mpegts_program_tables *tables, uint16_t number,
                                                           uint8_t tag, const uint8_t *data, size_t len);
mpegts_status mpegts_program_tables_add_stream(mpegts_program_tables *tables, uint16_t number, uint16_t pid,
                                               const mpegts_stream_spec *spec);
mpegts_status mpegts_program_tables_add_stream_descriptor(mpegts_program_tables *tables, uint16_t number,
                                                          uint16_t pid, uint8_t tag, const uint8_t *data,
                                                          size_t len);
mpegts_status mpegts_program_tables_remove_stream(mpegts_program_tables *tables, uint16_t number, uint16_t pid);
mpegts_status mpegts_program_tables_pat_version(const mpegts_program_tables *tables, uint8_t *version);
mpegts_status mpegts_program_tables_pmt_version(const mpegts_program_tables *tables, uint16_t number,
                                                uint8_t *version);
/* Set a version_number (0 to 31), e.g. to continue an input's numbering. */
mpegts_status mpegts_program_tables_set_pat_version(mpegts_program_tables *tables, uint8_t version);
mpegts_status mpegts_program_tables_set_pmt_version(mpegts_program_tables *tables, uint16_t number,
                                                    uint8_t version);
/* The sections themselves (at most 1024 bytes each). */
mpegts_status mpegts_program_tables_pat_section(mpegts_program_tables *tables, uint8_t *out, size_t capacity,
                                                size_t *written);
mpegts_status mpegts_program_tables_pmt_section(mpegts_program_tables *tables, uint16_t number, uint8_t *out,
                                                size_t capacity, size_t *written);
/* The tables as TS packets: the PAT on PID 0, a PMT on its PID, or the PAT
 * and every PMT. Repeat them at least every 100 ms (MISB ST 1402.2). */
mpegts_status mpegts_program_tables_write_pat(mpegts_program_tables *tables, uint8_t *out, size_t capacity,
                                              size_t *written);
mpegts_status mpegts_program_tables_write_pmt(mpegts_program_tables *tables, uint16_t number, uint8_t *out,
                                              size_t capacity, size_t *written);
mpegts_status mpegts_program_tables_write_all(mpegts_program_tables *tables, uint8_t *out, size_t capacity,
                                              size_t *written);
/* Continuity counter (0 to 15) of the next packet on PID 0 or a PMT PID. */
mpegts_status mpegts_program_tables_continuity_counter(const mpegts_program_tables *tables, uint16_t pid,
                                                       uint8_t *cc);
mpegts_status mpegts_program_tables_set_continuity_counter(mpegts_program_tables *tables, uint16_t pid, uint8_t cc);

/* mpegts_pes_unit.flags */
#define MPEGTS_PES_HAS_PTS 0x01
#define MPEGTS_PES_HAS_DTS 0x02        /* needs HAS_PTS; omitted when equal to the PTS */
#define MPEGTS_PES_HAS_PCR 0x04        /* PCR in the first TS packet */
#define MPEGTS_PES_DATA_ALIGNMENT 0x08 /* CARRIAGE_PES only; KLV sets its own */
#define MPEGTS_PES_RANDOM_ACCESS 0x10  /* random_access_indicator in the first TS packet */
#define MPEGTS_PES_ES_PRIORITY 0x20    /* elementary_stream_priority_indicator */
#define MPEGTS_PES_HAS_SERVICE_ID 0x40 /* METADATA_CELLS: service_id instead of the stream's */

/* One unit (access unit, KLV packet, any payload) to write. */
typedef struct mpegts_pes_unit {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_PES_* */
    uint64_t pts;   /* 90 kHz, with HAS_PTS (taken modulo 2^33) */
    uint64_t dts;   /* 90 kHz, with HAS_DTS */
    uint64_t pcr;   /* 27 MHz, with HAS_PCR */
    const uint8_t *data; /* may be NULL when data_len is 0 */
    uint64_t data_len;
    uint8_t service_id; /* with HAS_SERVICE_ID */
    uint8_t reserved[7];
} mpegts_pes_unit;

/* A writer for one elementary stream on `pid` (0x0010 to 0x1FFE). */
mpegts_status mpegts_pes_writer_new(uint16_t pid, const mpegts_stream_spec *spec, mpegts_pes_writer **out);
void mpegts_pes_writer_free(mpegts_pes_writer *writer); /* NULL is ignored */
mpegts_status mpegts_pes_writer_error(const mpegts_pes_writer *writer, char *message, size_t message_len);
/* Write one unit as PES packets in TS packets. A unit that breaks its
 * carriage's rules (sync KLV without a PTS, async KLV with one, a PCR on a
 * KLV PID, an empty KLV unit, ...) gives MPEGTS_ERR_INVALID; sync KLV units
 * over 65,522 bytes are fragmented into one cell per PES, each with the
 * unit's PTS; async KLV units over 65,532 bytes are split. */
mpegts_status mpegts_pes_writer_write(mpegts_pes_writer *writer, const mpegts_pes_unit *unit, uint8_t *out,
                                      size_t capacity, size_t *written);
/* One packet with an adaptation field and no payload, e.g. a PCR between
 * frames. flags: MPEGTS_PES_HAS_PCR, RANDOM_ACCESS, ES_PRIORITY (refused on
 * KLV PIDs). */
mpegts_status mpegts_pes_writer_write_adaptation_only(mpegts_pes_writer *writer, uint32_t flags, uint64_t pcr,
                                                      uint8_t *out, size_t capacity, size_t *written);
/* Continuity counter (0 to 15) of the next packet with payload. After
 * setting one that breaks the sequence, mark a discontinuity. */
mpegts_status mpegts_pes_writer_continuity_counter(const mpegts_pes_writer *writer, uint8_t *cc);
mpegts_status mpegts_pes_writer_set_continuity_counter(mpegts_pes_writer *writer, uint8_t cc);
/* Set discontinuity_indicator in the next packet. */
mpegts_status mpegts_pes_writer_mark_discontinuity(mpegts_pes_writer *writer);
/* sequence_number of the next metadata access unit cell. */
mpegts_status mpegts_pes_writer_sequence_number(const mpegts_pes_writer *writer, uint8_t *sequence);
mpegts_status mpegts_pes_writer_set_sequence_number(mpegts_pes_writer *writer, uint8_t sequence);

/* A null packet (PID 0x1FFF) into packet[0..188). */
mpegts_status mpegts_null_packet(uint8_t *packet);

/* ------------------------------------------------------------ injection
 *
 * The KLV injector (libraries built with `mux`): copies an input transport
 * stream through and inserts MISB ST 1402.2 KLV into one of its programs,
 * synchronous (stream_type 0x15, a PTS per access unit) or asynchronous
 * (stream_type 0x06). Only PID 0 (the input's PAT, resent with the
 * injector's counter), the target PMT PID (the PMT rewritten with the KLV
 * entry, version + 1) and the KLV PID change; PAT and PMT copies are added
 * so neither is more than psi_interval_ms apart. Synchronous units go out
 * in PTS order within [PTS - max_lead_ms, PTS - min_lead_ms], right before
 * the video frame they describe. In MPEGTS_RATE_CBR_* the output has as
 * many packets as the input: inserted packets replace null packets and the
 * packets the injector consumes, and no packet moves. Input is held until
 * the target's PMT is known (at most max_held_packets packets) and, for
 * mpegts_injector_push, until each PCR interval is complete. */

typedef struct mpegts_injector mpegts_injector;           /* opaque */
typedef struct mpegts_inject_units mpegts_inject_units;   /* opaque, frame callback only */

/* mpegts_inject_config.carriage */
#define MPEGTS_INJECT_SYNC 1
#define MPEGTS_INJECT_ASYNC 2
/* mpegts_inject_config.existing_klv: when the target already carries KLV */
#define MPEGTS_EXISTING_KLV_KEEP 0    /* keep it; sync joins an existing sync stream as a
                                       * new service (ST 1402-13), else a new PID */
#define MPEGTS_EXISTING_KLV_REPLACE 1 /* remove it (a PCR on it is kept) */
#define MPEGTS_EXISTING_KLV_FAIL 2    /* MPEGTS_ERR_STREAM */
/* mpegts_inject_config.rate */
#define MPEGTS_RATE_VBR 0             /* inserted packets are added */
#define MPEGTS_RATE_CBR_FAIL 1        /* same packet count; no room in time: MPEGTS_ERR_STREAM */
#define MPEGTS_RATE_CBR_DROP_OLDEST 2 /* same packet count; no room: drop units, count them */
/* mpegts_inject_config.flags */
#define MPEGTS_INJECT_HAS_SERVICE_ID 0x01    /* use service_id (else the lowest free one) */
#define MPEGTS_INJECT_HAS_PID 0x02           /* use pid (else a free one) */
#define MPEGTS_INJECT_NO_FRAME_ALIGN 0x04    /* send sync units as their window opens */
#define MPEGTS_INJECT_SYNC_REGISTRATION 0x08 /* also the "KLVA" registration_descriptor */

/* Settings. Fill with mpegts_inject_config_default, then change fields. */
typedef struct mpegts_inject_config {
    uint32_t struct_size;
    uint32_t flags;            /* MPEGTS_INJECT_* */
    uint32_t program;          /* program_number, or 0: the first program with video */
    uint32_t carriage;         /* MPEGTS_INJECT_SYNC or _ASYNC */
    uint32_t existing_klv;     /* MPEGTS_EXISTING_KLV_* */
    uint32_t rate;             /* MPEGTS_RATE_* */
    uint16_t pid;              /* with MPEGTS_INJECT_HAS_PID */
    uint8_t service_id;        /* with MPEGTS_INJECT_HAS_SERVICE_ID (sync only) */
    uint8_t reserved;
    uint32_t psi_interval_ms;  /* 1 to 249; default 100 */
    uint32_t max_lead_ms;      /* at most 1000; default 990 */
    uint32_t min_lead_ms;      /* at least 1; default 1 */
    uint32_t fallback_lead_ms; /* send a unit whose frame has not come this close to its PTS; 50 */
    uint32_t input_leak_rate;  /* metadata_STD_descriptor (sync): 0x3FFFFF, 0x400, 0 */
    uint32_t buffer_size;
    uint32_t output_leak_rate;
    uint64_t max_queued_units; /* default 4096 */
    uint64_t max_queued_bytes; /* default 8 MiB */
    uint64_t max_held_packets; /* default 50,000 */
} mpegts_inject_config;

/* mpegts_inject_unit.flags */
#define MPEGTS_INJECT_UNIT_HAS_PTS 0x01 /* required for sync, refused for async */

/* One KLV unit (a whole KLV packet) to insert. The bytes are copied. */
typedef struct mpegts_inject_unit {
    uint32_t struct_size;
    uint32_t flags;      /* MPEGTS_INJECT_UNIT_* */
    uint64_t pts;        /* 90 kHz, taken modulo 2^33 */
    const uint8_t *data; /* may be NULL when data_len is 0 (then refused) */
    uint64_t data_len;
} mpegts_inject_unit;

/* mpegts_inject_frame.flags */
#define MPEGTS_FRAME_HAS_DTS 0x01
#define MPEGTS_FRAME_HAS_CLOCK 0x02
#define MPEGTS_FRAME_RANDOM_ACCESS 0x04 /* random_access_indicator of its first packet */
#define MPEGTS_FRAME_DISCONTINUITY 0x08 /* first frame after a time base discontinuity */

/* A video frame of the target, about to be written. */
typedef struct mpegts_inject_frame {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_FRAME_* */
    uint16_t pid;
    uint16_t reserved;
    uint32_t reserved2;
    uint64_t pts;   /* 90 kHz */
    uint64_t dts;   /* with HAS_DTS */
    uint64_t clock; /* program clock, 27 MHz, with HAS_CLOCK */
} mpegts_inject_frame;

/* mpegts_inject_target.flags */
#define MPEGTS_TARGET_MERGED 0x01         /* joins an existing sync KLV stream on klv_pid */
#define MPEGTS_TARGET_HAS_SERVICE_ID 0x02

/* The program written into, and how. PIDs of 0x1FFF mean none. */
typedef struct mpegts_inject_target {
    uint32_t struct_size;
    uint32_t flags; /* MPEGTS_TARGET_* */
    uint16_t program;
    uint16_t pmt_pid;
    uint16_t pcr_pid;
    uint16_t video_pid;
    uint16_t klv_pid;
    uint8_t service_id;
    uint8_t carriage;        /* MPEGTS_INJECT_SYNC or _ASYNC */
    uint32_t stripped_count; /* input KLV PIDs removed (REPLACE) */
    uint16_t stripped[8];    /* the first eight of them */
} mpegts_inject_target;

/* Counters; see the Rust documentation of InjectStats for each. */
typedef struct mpegts_inject_stats {
    uint32_t struct_size;
    uint32_t reserved;
    uint64_t packets_in;
    uint64_t packets_out;
    uint64_t bytes_skipped;
    uint64_t sync_losses;
    uint64_t held_packets;
    uint64_t frames;
    uint64_t units_inserted;
    uint64_t units_queued;
    uint64_t bytes_queued;
    uint64_t klv_packets;
    uint64_t psi_packets;
    uint64_t psi_extra_packets;
    uint64_t stripped_packets;
    uint64_t pcr_only_packets;
    uint64_t null_packets;
    uint64_t free_slots;
    uint64_t slots_filled;
    uint64_t dropped_late;
    uint64_t dropped_overflow;
    uint64_t dropped_discontinuity;
    uint64_t dropped_unplaced;
    uint64_t late_units;
    uint64_t psi_crc_errors;
    uint64_t metadata_buffer_peak; /* bytes */
    uint64_t null_bitrate;         /* bit/s */
    uint64_t free_bitrate;         /* bit/s: CBR headroom */
    int64_t added_bitrate;         /* bit/s */
    uint64_t events_dropped;
} mpegts_inject_stats;

/* mpegts_inject_event.kind */
#define MPEGTS_EVENT_TARGET_FOUND 1   /* value: program_number */
#define MPEGTS_EVENT_TARGET_CHANGED 2 /* value: program_number */
#define MPEGTS_EVENT_TARGET_LOST 3    /* value: program_number */
#define MPEGTS_EVENT_DISCONTINUITY 4  /* value: first PCR of the new time base */
#define MPEGTS_EVENT_DROPPED 5        /* code: MPEGTS_DROP_*, value: PTS, bytes: size */
#define MPEGTS_EVENT_WARNING 6        /* code: MPEGTS_WARN_* */
/* mpegts_inject_event.flags */
#define MPEGTS_EVENT_HAS_PTS 0x01
/* Drop reasons */
#define MPEGTS_DROP_LATE 1          /* could no longer arrive before its PTS */
#define MPEGTS_DROP_OVERFLOW 2      /* no CBR room (MPEGTS_RATE_CBR_DROP_OLDEST) */
#define MPEGTS_DROP_DISCONTINUITY 3 /* across a time base discontinuity */
#define MPEGTS_DROP_UNPLACED 4      /* still queued at the end */
/* Warnings */
#define MPEGTS_WARN_MULTI_PROGRAM_VBR 1   /* value: programs; prefer CBR */
#define MPEGTS_WARN_SERVICE_ID_CHANGED 2  /* value: new id, bytes: old id */
#define MPEGTS_WARN_SERVICE_ID_IN_INPUT 3 /* value: the id */
#define MPEGTS_WARN_LATE_UNIT 4           /* value: PTS (CBR, DROP_OLDEST) */

typedef struct mpegts_inject_event {
    uint32_t struct_size;
    uint32_t kind;  /* MPEGTS_EVENT_* */
    uint32_t code;
    uint32_t flags; /* MPEGTS_EVENT_* flags */
    uint64_t value;
    uint64_t bytes;
} mpegts_inject_event;

/* Output: whole packets, len a multiple of 188, valid during the call. */
typedef void (*mpegts_packets_fn)(void *user, const uint8_t *packets, size_t len);
/* Called right before the first packet of each video frame of the target is
 * written. Units inserted through `units` (valid during the call only) can
 * go out immediately before that frame. */
typedef void (*mpegts_frame_fn)(void *user, const mpegts_inject_frame *frame, mpegts_inject_units *units);

mpegts_status mpegts_inject_config_default(mpegts_inject_config *config);
/* MPEGTS_OK, or MPEGTS_ERR_INVALID with the reason in `message`. */
mpegts_status mpegts_inject_config_check(const mpegts_inject_config *config, char *message, size_t message_len);
/* config may be NULL for the defaults. */
mpegts_status mpegts_injector_new(const mpegts_inject_config *config, mpegts_injector **out);
void mpegts_injector_free(mpegts_injector *injector); /* NULL is ignored */
/* Why the last call failed (empty after a success). */
mpegts_status mpegts_injector_error(const mpegts_injector *injector, char *message, size_t message_len);
/* Push input bytes in any chunking. Output goes to `sink` (required) before
 * the call returns; on_frame may be NULL. Errors other than
 * MPEGTS_ERR_INVALID and MPEGTS_ERR_FULL are final. */
mpegts_status mpegts_injector_push(mpegts_injector *injector, const uint8_t *data, size_t len,
                                   mpegts_packets_fn sink, void *sink_user, mpegts_frame_fn on_frame,
                                   void *frame_user);
/* Push one 188-byte packet with its program clock (27 MHz; NULL: none yet),
 * for callers that time the input themselves. Do not mix with _push. */
mpegts_status mpegts_injector_push_packet(mpegts_injector *injector, const uint8_t *packet, const uint64_t *clock,
                                          mpegts_packets_fn sink, void *sink_user, mpegts_frame_fn on_frame,
                                          void *frame_user);
/* Queue a unit. MPEGTS_ERR_FULL: the queue is full, try again after more
 * input; MPEGTS_ERR_INVALID: the unit breaks the carriage's rules, or a
 * sync PTS is below the last one sent. */
mpegts_status mpegts_injector_insert(mpegts_injector *injector, const mpegts_inject_unit *unit);
/* The same from inside the frame callback. */
mpegts_status mpegts_inject_units_insert(mpegts_inject_units *units, const mpegts_inject_unit *unit);
/* Program clock at the frame (MPEGTS_EMPTY before the first PCR). */
mpegts_status mpegts_inject_units_clock(const mpegts_inject_units *units, uint64_t *clock);
mpegts_status mpegts_inject_units_target(const mpegts_inject_units *units, mpegts_inject_target *target);
/* The input has ended: process the packets still held (frames among them
 * go to on_frame, which may be NULL) without ending the stream, so units
 * for the last frames can still be inserted before
 * mpegts_injector_finish. Pushing afterwards is MPEGTS_ERR_INVALID. */
mpegts_status mpegts_injector_end_input(mpegts_injector *injector, mpegts_packets_fn sink, void *sink_user,
                                        mpegts_frame_fn on_frame, void *frame_user);
/* End of input: flush, send what can still be placed, count the rest.
 * Frames still held (after the last PCR) go to on_frame (may be NULL).
 * stats may be NULL. */
mpegts_status mpegts_injector_finish(mpegts_injector *injector, mpegts_packets_fn sink, void *sink_user,
                                     mpegts_frame_fn on_frame, void *frame_user, mpegts_inject_stats *stats);
mpegts_status mpegts_injector_stats(const mpegts_injector *injector, mpegts_inject_stats *stats);
/* MPEGTS_EMPTY until the target program is chosen. */
mpegts_status mpegts_injector_target(const mpegts_injector *injector, mpegts_inject_target *target);
/* Program clock of the last input packet processed (MPEGTS_EMPTY before
 * the first PCR). */
mpegts_status mpegts_injector_clock(const mpegts_injector *injector, uint64_t *clock);
/* The oldest event; MPEGTS_EMPTY when there is none. */
mpegts_status mpegts_injector_pop_event(mpegts_injector *injector, mpegts_inject_event *event);

#ifdef __cplusplus
}
#endif

#endif /* MPEGTS_H */
