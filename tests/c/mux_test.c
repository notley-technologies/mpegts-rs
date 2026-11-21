/* Write-side ABI check, driven by tests/c_mux.rs.
 *
 *   mux_test            print struct layout
 *   mux_test OUT.ts     also write a synthetic stream to OUT.ts and print
 *                       what the calls returned
 *
 * The stream: program 1 with H.264 video on 0x100 (PCR), synchronous KLV
 * on 0x101 and asynchronous KLV on 0x102, five frames, one KLV access unit
 * large enough to be fragmented.
 */
#include "mpegts.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define S(s) printf("S %s %lu\n", #s, (unsigned long)sizeof(s));
#define F(s, f) printf("L %s.%s %lu\n", #s, #f, (unsigned long)offsetof(s, f));

static void layout(void) {
    S(mpegts_stream_spec)
    S(mpegts_pes_unit)
    F(mpegts_stream_spec, struct_size)
    F(mpegts_stream_spec, carriage)
    F(mpegts_stream_spec, stream_type)
    F(mpegts_stream_spec, stream_id)
    F(mpegts_stream_spec, service_id)
    F(mpegts_stream_spec, reserved)
    F(mpegts_stream_spec, flags)
    F(mpegts_stream_spec, input_leak_rate)
    F(mpegts_stream_spec, buffer_size)
    F(mpegts_stream_spec, output_leak_rate)
    F(mpegts_pes_unit, struct_size)
    F(mpegts_pes_unit, flags)
    F(mpegts_pes_unit, pts)
    F(mpegts_pes_unit, dts)
    F(mpegts_pes_unit, pcr)
    F(mpegts_pes_unit, data)
    F(mpegts_pes_unit, data_len)
    F(mpegts_pes_unit, service_id)
    F(mpegts_pes_unit, reserved)
}

static int check(const char *what, mpegts_status status) {
    if (status < 0) {
        fprintf(stderr, "%s: %s\n", what, mpegts_status_string(status));
        exit(1);
    }
    return status;
}

/* Output collected in memory, then written to the file. */
static uint8_t *ts;
static size_t ts_len, ts_cap;

static uint8_t *room(size_t n) {
    if (ts_len + n > ts_cap) {
        ts_cap = (ts_len + n) * 2;
        ts = realloc(ts, ts_cap);
        if (!ts) {
            exit(1);
        }
    }
    return ts + ts_len;
}

/* Write a unit: ask for the size, then write into exactly that much. */
static void write_unit(mpegts_pes_writer *w, const mpegts_pes_unit *unit) {
    size_t need = 0, written = 0;
    if (mpegts_pes_writer_write(w, unit, NULL, 0, &need) != MPEGTS_ERR_BUFFER) {
        fprintf(stderr, "size query did not report MPEGTS_ERR_BUFFER\n");
        exit(1);
    }
    check("pes_writer_write", mpegts_pes_writer_write(w, unit, room(need), need, &written));
    if (written != need || written % 188 != 0) {
        fprintf(stderr, "wrote %lu of %lu\n", (unsigned long)written, (unsigned long)need);
        exit(1);
    }
    ts_len += written;
}

static void write_tables(mpegts_program_tables *t) {
    size_t need = 0, written = 0;
    mpegts_program_tables_write_all(t, NULL, 0, &need);
    check("write_all", mpegts_program_tables_write_all(t, room(need), need, &written));
    ts_len += written;
}

/* A fake ST 0601 packet: the 16-byte key, a BER length and filler. */
static size_t klv(uint8_t *out, size_t value_len, uint8_t fill) {
    static const uint8_t key[16] = {0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01,
                                    0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00};
    size_t n = 0;
    memcpy(out, key, 16);
    n = 16;
    out[n++] = 0x83;
    out[n++] = (uint8_t)(value_len >> 16);
    out[n++] = (uint8_t)(value_len >> 8);
    out[n++] = (uint8_t)value_len;
    memset(out + n, fill, value_len);
    return n + value_len;
}

int main(int argc, char **argv) {
    mpegts_program_tables *tables = NULL, *copy = NULL;
    mpegts_pes_writer *video = NULL, *sync = NULL, *async = NULL;
    mpegts_stream_spec spec;
    mpegts_pes_unit unit;
    mpegts_report *report = NULL;
    static uint8_t frame[3000], packet[70100], small[188], pat_a[1024], pat_b[1024];
    static const uint8_t aud[6] = {0x00, 0x00, 0x00, 0x01, 0x09, 0xF0}; /* access unit delimiter */
    size_t need = 0, written = 0, a_len = 0, b_len = 0;
    char message[256];
    uint8_t value = 0;
    int i;
    FILE *f;

    layout();
    if (argc < 2) {
        return 0;
    }

    check("tables_new", mpegts_program_tables_new(7, &tables));
    check("add_program", mpegts_program_tables_add_program(tables, 1, 0x1000, 0x100));
    spec.struct_size = sizeof spec;
    check("spec_pes", mpegts_stream_spec_pes(&spec, 0x1B, 0xE0));
    check("add video", mpegts_program_tables_add_stream(tables, 1, 0x100, &spec));
    check("video writer", mpegts_pes_writer_new(0x100, &spec, &video));
    spec.struct_size = sizeof spec;
    check("spec_klv_sync", mpegts_stream_spec_klv_sync(&spec, 0));
    printf("SPEC sync carriage=%u type=0x%02x id=0x%02x rate=0x%06x buffer=0x%06x\n", spec.carriage,
           spec.stream_type, spec.stream_id, spec.input_leak_rate, spec.buffer_size);
    check("add sync", mpegts_program_tables_add_stream(tables, 1, 0x101, &spec));
    check("sync writer", mpegts_pes_writer_new(0x101, &spec, &sync));
    spec.struct_size = sizeof spec;
    check("spec_klv_async", mpegts_stream_spec_klv_async(&spec));
    check("add async", mpegts_program_tables_add_stream(tables, 1, 0x102, &spec));
    check("async writer", mpegts_pes_writer_new(0x102, &spec, &async));

    /* Refusals leave a reason and change nothing. */
    printf("DUP %d\n", mpegts_program_tables_add_stream(tables, 1, 0x102, &spec));
    check("error", mpegts_program_tables_error(tables, message, sizeof message));
    printf("ERR %s\n", message);
    printf("MISSING %d\n", mpegts_program_tables_remove_stream(tables, 9, 0x102));
    check("pmt_version", mpegts_program_tables_pmt_version(tables, 1, &value));
    printf("PMT_VERSION %u\n", value);
    check("pat_version", mpegts_program_tables_pat_version(tables, &value));
    printf("PAT_VERSION %u\n", value);

    /* The buffer rule: a size query, then a short buffer. */
    printf("QUERY %d", mpegts_program_tables_write_all(tables, NULL, 0, &need));
    printf(" %lu\n", (unsigned long)need);
    printf("SHORT %d", mpegts_program_tables_write_all(tables, small, sizeof small, &written));
    printf(" %lu\n", (unsigned long)written);
    check("cc", mpegts_program_tables_continuity_counter(tables, 0, &value));
    printf("PAT_CC %u\n", value);

    for (i = 0; i < 5; i++) {
        uint64_t pts = 126000 + (uint64_t)i * 3003;
        size_t n;
        write_tables(tables);

        memset(&unit, 0, sizeof unit);
        unit.struct_size = sizeof unit;
        memset(frame, 0xA0 + i, sizeof frame);
        memcpy(frame, aud, sizeof aud);
        unit.flags = MPEGTS_PES_HAS_PTS | MPEGTS_PES_HAS_DTS | MPEGTS_PES_HAS_PCR | MPEGTS_PES_RANDOM_ACCESS;
        unit.pts = pts + 3003;
        unit.dts = pts;
        unit.pcr = (pts - 9000) * 300;
        unit.data = frame;
        unit.data_len = sizeof frame;
        write_unit(video, &unit);

        n = klv(packet, i == 2 ? 70000 : 100 + (size_t)i, (uint8_t)i);
        memset(&unit, 0, sizeof unit);
        unit.struct_size = sizeof unit;
        unit.flags = MPEGTS_PES_HAS_PTS;
        unit.pts = pts + 3003;
        unit.data = packet;
        unit.data_len = n;
        write_unit(sync, &unit);

        n = klv(packet, 50 + (size_t)i, (uint8_t)(0x40 + i));
        unit.flags = 0;
        unit.data_len = n;
        write_unit(async, &unit);
    }

    /* Rule checks on units. */
    unit.flags = MPEGTS_PES_HAS_PTS;
    printf("ASYNC_PTS %d\n", mpegts_pes_writer_write(async, &unit, small, sizeof small, &written));
    unit.flags = 0;
    printf("SYNC_NO_PTS %d\n", mpegts_pes_writer_write(sync, &unit, small, sizeof small, &written));
    check("error", mpegts_pes_writer_error(sync, message, sizeof message));
    printf("ERR %s\n", message);
    check("sequence", mpegts_pes_writer_sequence_number(sync, &value));
    printf("SEQ %u\n", value);
    check("cc", mpegts_pes_writer_continuity_counter(video, &value));
    check("adaptation_only", mpegts_pes_writer_write_adaptation_only(video, MPEGTS_PES_HAS_PCR, 27000000ull * 2,
                                                                      room(188), 188, &written));
    ts_len += written;
    check("null", mpegts_null_packet(room(188)));
    ts_len += 188;

    /* Tables rebuilt from what the demuxer reports give the same PAT. */
    check("analyze", mpegts_analyze(ts, ts_len, &report));
    check("from_report", mpegts_program_tables_from_report(report, &copy));
    check("pat a", mpegts_program_tables_pat_section(tables, pat_a, sizeof pat_a, &a_len));
    check("pat b", mpegts_program_tables_pat_section(copy, pat_b, sizeof pat_b, &b_len));
    printf("FROM_REPORT %s\n", a_len == b_len && memcmp(pat_a, pat_b, a_len) == 0 ? "same" : "different");
    check("pmt a", mpegts_program_tables_pmt_section(tables, 1, pat_a, sizeof pat_a, &a_len));
    check("pmt b", mpegts_program_tables_pmt_section(copy, 1, pat_b, sizeof pat_b, &b_len));
    printf("FROM_REPORT_PMT %s %lu\n", a_len == b_len && memcmp(pat_a, pat_b, a_len) == 0 ? "same" : "different",
           (unsigned long)a_len);
    mpegts_report_free(report);
    mpegts_program_tables_free(copy);

    f = fopen(argv[1], "wb");
    if (!f || fwrite(ts, 1, ts_len, f) != ts_len || fclose(f) != 0) {
        perror(argv[1]);
        return 1;
    }
    printf("WROTE %lu\n", (unsigned long)ts_len);
    mpegts_pes_writer_free(video);
    mpegts_pes_writer_free(sync);
    mpegts_pes_writer_free(async);
    mpegts_program_tables_free(tables);
    free(ts);
    printf("DONE\n");
    return 0;
}
