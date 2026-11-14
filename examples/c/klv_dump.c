/* Print every KLV unit in a transport stream: PID, PTS and size.
 *
 * Against an installed library (`make install`):
 *
 *   cc klv_dump.c $(pkg-config --cflags --libs mpegts) -o klv_dump
 *
 * or with CMake, see CMakeLists.txt next to this file. Straight from the
 * build tree:
 *
 *   cargo build --release
 *   cc -I include examples/c/klv_dump.c -L target/release -lmpegts -o klv_dump
 *   LD_LIBRARY_PATH=target/release ./klv_dump capture.ts
 */
#include "mpegts.h"

#include <stdio.h>

static int fail(const char *what, mpegts_status status) {
    fprintf(stderr, "%s: %s\n", what, mpegts_status_string(status));
    return 1;
}

/* Print and release every output waiting in the queue. */
static mpegts_status drain(mpegts_demuxer *d) {
    mpegts_output out;
    mpegts_status s;
    for (;;) {
        out.struct_size = sizeof out;
        s = mpegts_demuxer_next_output(d, &out);
        if (s != MPEGTS_OK) {
            return s == MPEGTS_EMPTY ? MPEGTS_OK : s;
        }
        /* The KLV itself: out.data + out.payload_offset, valid until the
         * next call to mpegts_demuxer_next_output. */
        printf("pid 0x%04x  %llu bytes", out.pid, (unsigned long long)(out.data_len - out.payload_offset));
        if (out.flags & MPEGTS_OUTPUT_HAS_PTS) {
            printf("  pts %llu (%.3f s)", (unsigned long long)out.pts, out.pts / 90000.0);
        }
        printf("%s\n", (out.flags & MPEGTS_OUTPUT_DAMAGED) ? "  damaged" : "");
    }
}

int main(int argc, char **argv) {
    mpegts_demuxer *d = NULL;
    /* strip_metadata_au = 1: sync KLV comes without its access unit cell
     * headers, so both kinds of KLV arrive as plain KLV. */
    mpegts_subscription sub = {sizeof(mpegts_subscription), MPEGTS_SELECT_CODEC, 0, MPEGTS_UNIT_PES, 0, 1};
    unsigned char buf[1316]; /* one UDP datagram's worth */
    uint32_t id;
    size_t n;
    mpegts_status s;
    FILE *f;

    if (argc != 2) {
        fprintf(stderr, "usage: %s FILE.ts\n", argv[0]);
        return 2;
    }
    if (mpegts_abi_version() != MPEGTS_ABI_VERSION) {
        fprintf(stderr, "libmpegts has ABI %u, this program needs %d\n", mpegts_abi_version(), MPEGTS_ABI_VERSION);
        return 1;
    }
    if ((s = mpegts_demuxer_new(NULL, &d)) != MPEGTS_OK) {
        return fail("new", s);
    }
    sub.value = MPEGTS_CODEC_KLV_ASYNC;
    if ((s = mpegts_demuxer_subscribe(d, &sub, &id)) != MPEGTS_OK) {
        return fail("subscribe", s);
    }
    sub.value = MPEGTS_CODEC_KLV_SYNC;
    if ((s = mpegts_demuxer_subscribe(d, &sub, &id)) != MPEGTS_OK) {
        return fail("subscribe", s);
    }
    f = fopen(argv[1], "rb");
    if (!f) {
        perror(argv[1]);
        return 1;
    }
    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        if ((s = mpegts_demuxer_push(d, buf, n)) != MPEGTS_OK || (s = drain(d)) != MPEGTS_OK) {
            fclose(f);
            return fail("push", s);
        }
    }
    fclose(f);
    if ((s = mpegts_demuxer_finish(d)) != MPEGTS_OK || (s = drain(d)) != MPEGTS_OK) {
        return fail("finish", s);
    }
    mpegts_demuxer_free(d);
    return 0;
}
