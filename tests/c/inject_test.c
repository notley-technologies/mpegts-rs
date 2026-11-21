/* Injector ABI check, driven by tests/c_mux.rs.
 *
 *   inject_test                  print struct layout
 *   inject_test IN.ts OUT.ts     also inject synchronous KLV into IN.ts,
 *                                one unit per video frame from the frame
 *                                callback, write OUT.ts and print what the
 *                                calls returned
 *   inject_test IN.ts OUT.ts SEI_IN.ts SEI_OUT.ts
 *                                also read and write SEI user data in the
 *                                H.264 video of SEI_IN.ts (MPEGTS_VIDEO_SEI_WRITE),
 *                                then inject it as M2TS records
 */
#include "mpegts.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define S(s) printf("S %s %lu\n", #s, (unsigned long)sizeof(s));
#define F(s, f) printf("L %s.%s %lu\n", #s, #f, (unsigned long)offsetof(s, f));

static void layout(void) {
    S(mpegts_inject_config)
    S(mpegts_inject_unit)
    S(mpegts_inject_frame)
    S(mpegts_inject_target)
    S(mpegts_inject_stats)
    S(mpegts_inject_event)
    F(mpegts_inject_config, struct_size)
    F(mpegts_inject_config, flags)
    F(mpegts_inject_config, program)
    F(mpegts_inject_config, carriage)
    F(mpegts_inject_config, existing_klv)
    F(mpegts_inject_config, rate)
    F(mpegts_inject_config, pid)
    F(mpegts_inject_config, service_id)
    F(mpegts_inject_config, reserved)
    F(mpegts_inject_config, psi_interval_ms)
    F(mpegts_inject_config, max_lead_ms)
    F(mpegts_inject_config, min_lead_ms)
    F(mpegts_inject_config, fallback_lead_ms)
    F(mpegts_inject_config, input_leak_rate)
    F(mpegts_inject_config, buffer_size)
    F(mpegts_inject_config, output_leak_rate)
    F(mpegts_inject_config, max_queued_units)
    F(mpegts_inject_config, max_queued_bytes)
    F(mpegts_inject_config, max_held_packets)
    F(mpegts_inject_config, video_sei)
    F(mpegts_inject_config, clock)
    F(mpegts_inject_config, record)
    F(mpegts_inject_config, video_sei_max_prefix)
    F(mpegts_inject_unit, struct_size)
    F(mpegts_inject_unit, flags)
    F(mpegts_inject_unit, pts)
    F(mpegts_inject_unit, data)
    F(mpegts_inject_unit, data_len)
    F(mpegts_inject_frame, struct_size)
    F(mpegts_inject_frame, flags)
    F(mpegts_inject_frame, pid)
    F(mpegts_inject_frame, reserved)
    F(mpegts_inject_frame, reserved2)
    F(mpegts_inject_frame, pts)
    F(mpegts_inject_frame, dts)
    F(mpegts_inject_frame, clock)
    F(mpegts_inject_frame, video_codec)
    F(mpegts_inject_frame, user_data_count)
    F(mpegts_inject_frame, prefix_bytes)
    F(mpegts_inject_target, struct_size)
    F(mpegts_inject_target, flags)
    F(mpegts_inject_target, program)
    F(mpegts_inject_target, pmt_pid)
    F(mpegts_inject_target, pcr_pid)
    F(mpegts_inject_target, video_pid)
    F(mpegts_inject_target, klv_pid)
    F(mpegts_inject_target, service_id)
    F(mpegts_inject_target, carriage)
    F(mpegts_inject_target, stripped_count)
    F(mpegts_inject_target, stripped)
    F(mpegts_inject_target, video_codec)
    F(mpegts_inject_target, video_stream_type)
    F(mpegts_inject_target, reserved3)
    F(mpegts_inject_stats, struct_size)
    F(mpegts_inject_stats, reserved)
    F(mpegts_inject_stats, packets_in)
    F(mpegts_inject_stats, packets_out)
    F(mpegts_inject_stats, units_inserted)
    F(mpegts_inject_stats, metadata_buffer_peak)
    F(mpegts_inject_stats, added_bitrate)
    F(mpegts_inject_stats, events_dropped)
    F(mpegts_inject_stats, frames_scanned)
    F(mpegts_inject_stats, user_data_found)
    F(mpegts_inject_stats, user_data_inserted)
    F(mpegts_inject_stats, user_data_rewritten)
    F(mpegts_inject_stats, user_data_skipped)
    F(mpegts_inject_stats, video_packets_added)
    F(mpegts_inject_stats, video_slots_filled)
    F(mpegts_inject_event, struct_size)
    F(mpegts_inject_event, kind)
    F(mpegts_inject_event, code)
    F(mpegts_inject_event, flags)
    F(mpegts_inject_event, value)
    F(mpegts_inject_event, bytes)
}

static void check(const char *what, mpegts_status status) {
    if (status < 0) {
        fprintf(stderr, "%s: %s\n", what, mpegts_status_string(status));
        exit(1);
    }
}

/* Output collected in memory. */
struct buffer {
    uint8_t *data;
    size_t len, cap;
};

static void collect(void *user, const uint8_t *packets, size_t len) {
    struct buffer *b = user;
    if (len % 188 != 0) {
        fprintf(stderr, "sink got %lu bytes\n", (unsigned long)len);
        exit(1);
    }
    if (b->len + len > b->cap) {
        b->cap = (b->len + len) * 2;
        b->data = realloc(b->data, b->cap);
        if (!b->data) {
            exit(1);
        }
    }
    memcpy(b->data + b->len, packets, len);
    b->len += len;
}

static uint8_t *read_file(const char *path, size_t *len);

/* M2TS output: whole 192-byte records. */
static void collect_records(void *user, const uint8_t *records, size_t len) {
    struct buffer *b = user;
    if (len % 192 != 0) {
        fprintf(stderr, "record sink got %lu bytes\n", (unsigned long)len);
        exit(1);
    }
    if (b->len + len > b->cap) {
        b->cap = (b->len + len) * 2;
        b->data = realloc(b->data, b->cap);
        if (!b->data) {
            exit(1);
        }
    }
    memcpy(b->data + b->len, records, len);
    b->len += len;
}

struct frames {
    unsigned count, with_clock, inserted;
    uint16_t klv_pid;
};

/* One fake ST 0601 packet per frame, stamped with the frame's PTS. */
static void on_frame(void *user, const mpegts_inject_frame *frame, mpegts_inject_units *units) {
    struct frames *f = user;
    static const uint8_t key[16] = {0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01,
                                    0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00};
    uint8_t klv[16 + 1 + 8];
    mpegts_inject_unit unit;
    mpegts_inject_target target;
    uint64_t clock = 0;
    int i;
    memcpy(klv, key, 16);
    klv[16] = 8;
    for (i = 0; i < 8; i++) {
        klv[17 + i] = (uint8_t)(frame->pts >> (56 - 8 * i));
    }
    memset(&unit, 0, sizeof unit);
    unit.struct_size = sizeof unit;
    unit.flags = MPEGTS_INJECT_UNIT_HAS_PTS;
    unit.pts = frame->pts;
    unit.data = klv;
    unit.data_len = sizeof klv;
    f->count++;
    if (mpegts_inject_units_clock(units, &clock) == MPEGTS_OK && (frame->flags & MPEGTS_FRAME_HAS_CLOCK) &&
        clock == frame->clock) {
        f->with_clock++;
    }
    target.struct_size = sizeof target;
    if (mpegts_inject_units_target(units, &target) == MPEGTS_OK) {
        f->klv_pid = target.klv_pid;
    }
    if (mpegts_inject_units_insert(units, &unit) == MPEGTS_OK) {
        f->inserted++;
    }
}

/* What the SEI frame callback saw. */
struct sei_frames {
    unsigned count, scanned, complete, h264, messages, misp, written, second, range;
};

static const uint8_t misp_uuid[16] = {'M', 'I', 'S', 'P', 'm', 'i', 'c', 'r', 'o', 's', 'e', 'c', 't', 'i', 'm', 'e'};

/* Write an ST 0604-like message into every access unit, replacing the one
 * there is. */
static void on_sei_frame(void *user, const mpegts_inject_frame *frame, mpegts_inject_units *units) {
    struct sei_frames *f = user;
    uint8_t payload[12] = {0x9F, 0, 0, 0xFF, 0, 0, 0xFF, 0, 0, 0xFF, 0, 0};
    uint8_t uuid[16];
    const uint8_t *data = NULL;
    uint64_t len = 0;
    uint32_t i;
    f->count++;
    if (frame->flags & MPEGTS_FRAME_SCANNED) {
        f->scanned++;
    }
    if (frame->flags & MPEGTS_FRAME_PREFIX_COMPLETE) {
        f->complete++;
    }
    if (frame->video_codec == MPEGTS_CODEC_H264) {
        f->h264++;
    }
    for (i = 0; i < frame->user_data_count; i++) {
        if (mpegts_inject_units_user_data(units, i, uuid, &data, &len) == MPEGTS_OK) {
            f->messages++;
            if (memcmp(uuid, misp_uuid, 16) == 0 && len == 12 && data != NULL) {
                f->misp++;
            }
        }
    }
    if (mpegts_inject_units_user_data(units, frame->user_data_count, uuid, &data, &len) == MPEGTS_ERR_RANGE) {
        f->range++;
    }
    payload[11] = (uint8_t)f->count;
    if (mpegts_inject_units_write_user_data(units, misp_uuid, payload, sizeof payload, misp_uuid, 1) ==
        MPEGTS_OK) {
        f->written++;
    }
    if (mpegts_inject_units_write_user_data(units, misp_uuid, payload, sizeof payload, NULL, 0) ==
        MPEGTS_ERR_INVALID) {
        f->second++;
    }
}

static void sei_phase(const char *in_path, const char *out_path) {
    mpegts_inject_config cfg;
    mpegts_injector *injector = NULL;
    mpegts_inject_stats stats;
    mpegts_inject_target target;
    struct buffer out = {NULL, 0, 0};
    struct sei_frames f;
    uint8_t *in, *m2ts;
    uint8_t record[192];
    size_t in_len, at, i, n;
    FILE *file;

    memset(&f, 0, sizeof f);
    in = read_file(in_path, &in_len);
    cfg.struct_size = sizeof cfg;
    check("config_default", mpegts_inject_config_default(&cfg));
    printf("SEI_DEFAULTS video_sei=%u clock=%u record=%u prefix=%u\n", cfg.video_sei, cfg.clock, cfg.record,
           cfg.video_sei_max_prefix);
    cfg.video_sei = MPEGTS_VIDEO_SEI_WRITE;
    cfg.clock = MPEGTS_CLOCK_EXTRAPOLATE;
    check("injector_new", mpegts_injector_new(&cfg, &injector));
    memset(record, 0, sizeof record);
    printf("RECORD_WRONG %d\n", mpegts_injector_push_record(injector, record, NULL, collect, &out, NULL, NULL));
    for (at = 0; at < in_len; at += 1316) {
        size_t len = in_len - at < 1316 ? in_len - at : 1316;
        check("push", mpegts_injector_push(injector, in + at, len, collect, &out, on_sei_frame, &f));
    }
    stats.struct_size = sizeof stats;
    check("finish", mpegts_injector_finish(injector, collect, &out, on_sei_frame, &f, &stats));
    printf("SEI frames=%u scanned=%u complete=%u h264=%u messages=%u misp=%u written=%u second=%u range=%u\n",
           f.count, f.scanned, f.complete, f.h264, f.messages, f.misp, f.written, f.second, f.range);
    printf("SEI_STATS scanned=%lu found=%lu inserted=%lu rewritten=%lu skipped=%lu\n",
           (unsigned long)stats.frames_scanned, (unsigned long)stats.user_data_found,
           (unsigned long)stats.user_data_inserted, (unsigned long)stats.user_data_rewritten,
           (unsigned long)stats.user_data_skipped);
    target.struct_size = sizeof target;
    check("target", mpegts_injector_target(injector, &target));
    printf("SEI_TARGET codec=0x%x stream_type=0x%02x\n", target.video_codec, target.video_stream_type);
    mpegts_injector_free(injector);
    file = fopen(out_path, "wb");
    if (!file || fwrite(out.data, 1, out.len, file) != out.len || fclose(file) != 0) {
        perror(out_path);
        exit(1);
    }

    /* The same input as M2TS records: their headers come back on the
     * records that pass through. */
    n = in_len / 188;
    m2ts = malloc(n * 192);
    if (!m2ts) {
        exit(1);
    }
    for (i = 0; i < n; i++) {
        uint32_t h = 0x40000000u | (uint32_t)(i * 1000);
        m2ts[i * 192] = (uint8_t)(h >> 24);
        m2ts[i * 192 + 1] = (uint8_t)(h >> 16);
        m2ts[i * 192 + 2] = (uint8_t)(h >> 8);
        m2ts[i * 192 + 3] = (uint8_t)h;
        memcpy(m2ts + i * 192 + 4, in + i * 188, 188);
    }
    check("config_default", mpegts_inject_config_default(&cfg));
    cfg.record = MPEGTS_RECORD_M2TS;
    check("injector_new", mpegts_injector_new(&cfg, &injector));
    printf("PACKET_WRONG %d\n", mpegts_injector_push_packet(injector, in, NULL, collect_records, &out, NULL, NULL));
    out.len = 0;
    for (i = 0; i < n; i++) {
        check("push_record",
              mpegts_injector_push_record(injector, m2ts + i * 192, NULL, collect_records, &out, NULL, NULL));
    }
    check("finish", mpegts_injector_finish(injector, collect_records, &out, NULL, NULL, &stats));
    /* Records that pass through keep their header: the first video one. */
    for (i = 0; i < out.len; i += 192) {
        if (((out.data[i + 5] & 0x1F) << 8 | out.data[i + 6]) == 0x100) {
            printf("M2TS first_video_header=%02x%02x%02x%02x\n", out.data[i], out.data[i + 1], out.data[i + 2],
                   out.data[i + 3]);
            break;
        }
    }
    printf("M2TS records=%lu in=%lu out=%lu\n", (unsigned long)(out.len / 192), (unsigned long)stats.packets_in,
           (unsigned long)stats.packets_out);
    mpegts_injector_free(injector);
    free(m2ts);
    free(out.data);
    free(in);
}

static uint8_t *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    uint8_t *data;
    long n;
    if (!f || fseek(f, 0, SEEK_END) != 0 || (n = ftell(f)) < 0 || fseek(f, 0, SEEK_SET) != 0) {
        perror(path);
        exit(1);
    }
    data = malloc((size_t)n + 1);
    if (!data || fread(data, 1, (size_t)n, f) != (size_t)n) {
        perror(path);
        exit(1);
    }
    fclose(f);
    *len = (size_t)n;
    return data;
}

int main(int argc, char **argv) {
    mpegts_inject_config cfg;
    mpegts_injector *injector = NULL, *bad = NULL;
    mpegts_inject_target target;
    mpegts_inject_stats stats;
    mpegts_inject_event event;
    mpegts_inject_unit unit;
    struct buffer out = {NULL, 0, 0};
    struct frames frames = {0, 0, 0, 0};
    char message[256];
    uint8_t *in;
    size_t in_len, at;
    uint64_t clock = 0;
    FILE *f;

    layout();
    if (argc < 3) {
        return 0;
    }
    in = read_file(argv[1], &in_len);

    cfg.struct_size = sizeof cfg;
    check("config_default", mpegts_inject_config_default(&cfg));
    printf("DEFAULTS psi=%u lead=%u..%u carriage=%u rate=%u units=%lu\n", cfg.psi_interval_ms, cfg.min_lead_ms,
           cfg.max_lead_ms, cfg.carriage, cfg.rate, (unsigned long)cfg.max_queued_units);

    /* A bad configuration is refused with a reason. */
    cfg.psi_interval_ms = 500;
    printf("CHECK %d", mpegts_inject_config_check(&cfg, message, sizeof message));
    printf(" %s\n", message);
    printf("NEW_BAD %d %s\n", mpegts_injector_new(&cfg, &bad), bad == NULL ? "null" : "set");
    cfg.psi_interval_ms = 100;

    check("injector_new", mpegts_injector_new(&cfg, &injector));
    printf("TARGET_BEFORE %d\n", mpegts_injector_target(injector, &target));
    printf("CLOCK_BEFORE %d\n", mpegts_injector_clock(injector, &clock));

    /* Units need a PTS for synchronous carriage. */
    memset(&unit, 0, sizeof unit);
    unit.struct_size = sizeof unit;
    unit.data = (const uint8_t *)"KLV";
    unit.data_len = 3;
    printf("NO_PTS %d\n", mpegts_injector_insert(injector, &unit));
    check("error", mpegts_injector_error(injector, message, sizeof message));
    printf("ERR %s\n", message);
    printf("NO_SINK %d\n", mpegts_injector_push(injector, in, 188, NULL, NULL, NULL, NULL));

    /* The input in UDP-sized pieces. */
    for (at = 0; at < in_len; at += 1316) {
        size_t n = in_len - at < 1316 ? in_len - at : 1316;
        check("push", mpegts_injector_push(injector, in + at, n, collect, &out, on_frame, &frames));
    }
    check("end_input", mpegts_injector_end_input(injector, collect, &out, on_frame, &frames));
    printf("PUSH_AFTER_END %d\n", mpegts_injector_push(injector, in, 188, collect, &out, on_frame, &frames));
    stats.struct_size = sizeof stats;
    check("finish", mpegts_injector_finish(injector, collect, &out, on_frame, &frames, &stats));
    printf("FRAMES %u clock=%u inserted=%u klv_pid=0x%04x\n", frames.count, frames.with_clock, frames.inserted,
           frames.klv_pid);
    printf("STATS in=%lu out=%lu units=%lu late=%lu out_bytes=%s\n", (unsigned long)stats.packets_in,
           (unsigned long)stats.packets_out, (unsigned long)stats.units_inserted, (unsigned long)stats.dropped_late,
           stats.packets_out * 188 == out.len ? "match" : "differ");
    target.struct_size = sizeof target;
    check("target", mpegts_injector_target(injector, &target));
    printf("TARGET program=%u pmt=0x%04x pcr=0x%04x video=0x%04x klv=0x%04x service=%u carriage=%u flags=%u\n",
           target.program, target.pmt_pid, target.pcr_pid, target.video_pid, target.klv_pid, target.service_id,
           target.carriage, target.flags);
    event.struct_size = sizeof event;
    while (mpegts_injector_pop_event(injector, &event) == MPEGTS_OK) {
        printf("EVENT kind=%u code=%u value=%lu\n", event.kind, event.code, (unsigned long)event.value);
    }
    check("clock", mpegts_injector_clock(injector, &clock));
    printf("CLOCK %s\n", clock > 0 ? "set" : "zero");
    printf("AFTER_FINISH %d\n", mpegts_injector_push(injector, in, 188, collect, &out, NULL, NULL));
    printf("STATUS %s / %s\n", mpegts_status_string(MPEGTS_ERR_FULL), mpegts_status_string(MPEGTS_ERR_STREAM));

    f = fopen(argv[2], "wb");
    if (!f || fwrite(out.data, 1, out.len, f) != out.len || fclose(f) != 0) {
        perror(argv[2]);
        return 1;
    }
    mpegts_injector_free(injector);
    mpegts_injector_free(NULL);
    free(out.data);
    free(in);
    if (argc >= 5) {
        sei_phase(argv[3], argv[4]);
    }
    printf("DONE\n");
    return 0;
}
