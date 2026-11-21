/* Injector ABI check, driven by tests/c_mux.rs.
 *
 *   inject_test                  print struct layout
 *   inject_test IN.ts OUT.ts     also inject synchronous KLV into IN.ts,
 *                                one unit per video frame from the frame
 *                                callback, write OUT.ts and print what the
 *                                calls returned
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
    F(mpegts_inject_stats, struct_size)
    F(mpegts_inject_stats, reserved)
    F(mpegts_inject_stats, packets_in)
    F(mpegts_inject_stats, packets_out)
    F(mpegts_inject_stats, units_inserted)
    F(mpegts_inject_stats, metadata_buffer_peak)
    F(mpegts_inject_stats, added_bitrate)
    F(mpegts_inject_stats, events_dropped)
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
    printf("DONE\n");
    return 0;
}
