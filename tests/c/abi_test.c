/* ABI conformance and end-to-end check, driven by tests/c_abi.rs.
 *
 *   abi_test            print struct layout
 *   abi_test FILE.ts    also analyse FILE.ts and print what was found
 */
#include "mpegts.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define S(s) printf("S %s %lu\n", #s, (unsigned long)sizeof(s));
#define F(s, f) printf("L %s.%s %lu\n", #s, #f, (unsigned long)offsetof(s, f));

static void layout(void) {
    S(mpegts_config)
    S(mpegts_subscription)
    S(mpegts_output)
    S(mpegts_output_stats)
    S(mpegts_report_summary)
    S(mpegts_program_info)
    S(mpegts_stream_info)
    S(mpegts_pid_info)
    F(mpegts_config, struct_size)
    F(mpegts_config, sync_confirmations)
    F(mpegts_config, probe_pes)
    F(mpegts_config, overflow)
    F(mpegts_config, max_streams)
    F(mpegts_config, history_packets)
    F(mpegts_config, queue_max_items)
    F(mpegts_config, queue_max_bytes)
    F(mpegts_config, max_pes_bytes)
    F(mpegts_config, max_reassembly_bytes)
    F(mpegts_subscription, struct_size)
    F(mpegts_subscription, select)
    F(mpegts_subscription, value)
    F(mpegts_subscription, unit)
    F(mpegts_subscription, replay)
    F(mpegts_subscription, strip_metadata_au)
    F(mpegts_output, struct_size)
    F(mpegts_output, subscription)
    F(mpegts_output, unit)
    F(mpegts_output, flags)
    F(mpegts_output, pid)
    F(mpegts_output, stream_id)
    F(mpegts_output, table_id)
    F(mpegts_output, reserved)
    F(mpegts_output, packet)
    F(mpegts_output, pts)
    F(mpegts_output, dts)
    F(mpegts_output, data)
    F(mpegts_output, data_len)
    F(mpegts_output, payload_offset)
    F(mpegts_output_stats, struct_size)
    F(mpegts_output_stats, subscriptions)
    F(mpegts_output_stats, queued)
    F(mpegts_output_stats, dropped)
    F(mpegts_output_stats, truncated)
    F(mpegts_output_stats, damaged)
    F(mpegts_output_stats, pending)
    F(mpegts_output_stats, pending_bytes)
    F(mpegts_output_stats, reassembly_bytes)
    F(mpegts_report_summary, struct_size)
    F(mpegts_report_summary, flags)
    F(mpegts_report_summary, revision)
    F(mpegts_report_summary, transport_stream_id)
    F(mpegts_report_summary, network_pid)
    F(mpegts_report_summary, pat_version)
    F(mpegts_report_summary, reserved)
    F(mpegts_report_summary, packet_size)
    F(mpegts_report_summary, program_count)
    F(mpegts_report_summary, pid_count)
    F(mpegts_report_summary, ca_system_count)
    F(mpegts_report_summary, packets)
    F(mpegts_report_summary, bytes_skipped)
    F(mpegts_report_summary, sync_losses)
    F(mpegts_report_summary, crc_errors)
    F(mpegts_report_summary, streams_dropped)
    F(mpegts_report_summary, duration_ns)
    F(mpegts_report_summary, bitrate)
    F(mpegts_program_info, struct_size)
    F(mpegts_program_info, flags)
    F(mpegts_program_info, number)
    F(mpegts_program_info, pmt_pid)
    F(mpegts_program_info, pcr_pid)
    F(mpegts_program_info, version)
    F(mpegts_program_info, reserved)
    F(mpegts_program_info, registration)
    F(mpegts_program_info, stream_count)
    F(mpegts_program_info, descriptor_count)
    F(mpegts_program_info, reserved2)
    F(mpegts_program_info, first_pcr)
    F(mpegts_program_info, last_pcr)
    F(mpegts_program_info, pcr_span)
    F(mpegts_stream_info, struct_size)
    F(mpegts_stream_info, flags)
    F(mpegts_stream_info, program)
    F(mpegts_stream_info, pid)
    F(mpegts_stream_info, stream_type)
    F(mpegts_stream_info, pes_stream_id)
    F(mpegts_stream_info, reserved)
    F(mpegts_stream_info, codec)
    F(mpegts_stream_info, kind)
    F(mpegts_stream_info, basis)
    F(mpegts_stream_info, language)
    F(mpegts_stream_info, registration)
    F(mpegts_stream_info, metadata_format)
    F(mpegts_stream_info, descriptor_count)
    F(mpegts_stream_info, reserved2)
    F(mpegts_stream_info, first_pts)
    F(mpegts_stream_info, last_pts)
    F(mpegts_stream_info, last_dts)
    F(mpegts_stream_info, pts_span)
    F(mpegts_stream_info, packets)
    F(mpegts_stream_info, cc_errors)
    F(mpegts_stream_info, scrambled_packets)
    F(mpegts_stream_info, last_packet)
    F(mpegts_stream_info, bitrate)
    F(mpegts_pid_info, struct_size)
    F(mpegts_pid_info, pid)
    F(mpegts_pid_info, reserved)
    F(mpegts_pid_info, packets)
    F(mpegts_pid_info, cc_errors)
    F(mpegts_pid_info, transport_errors)
    F(mpegts_pid_info, scrambled_packets)
    F(mpegts_pid_info, last_packet)
}

static int check(const char *what, mpegts_status status) {
    if (status < 0) {
        fprintf(stderr, "%s: %s\n", what, mpegts_status_string(status));
        exit(1);
    }
    return status;
}

static void drain(mpegts_demuxer *d) {
    mpegts_output out;
    for (;;) {
        out.struct_size = sizeof out;
        if (check("next_output", mpegts_demuxer_next_output(d, &out)) == MPEGTS_EMPTY) {
            return;
        }
        printf("OUT sub=%u unit=%u pid=%u pts=%llu has_pts=%d len=%llu payload=%llu first=%02x crc_ok=%d\n",
               out.subscription, out.unit, out.pid, (unsigned long long)out.pts,
               (out.flags & MPEGTS_OUTPUT_HAS_PTS) != 0, (unsigned long long)out.data_len,
               (unsigned long long)out.payload_offset,
               out.data_len > out.payload_offset ? out.data[out.payload_offset] : 0,
               (out.flags & MPEGTS_OUTPUT_CRC_OK) != 0);
    }
}

int main(int argc, char **argv) {
    mpegts_config config;
    mpegts_demuxer *d = NULL;
    mpegts_subscription klv = {sizeof(mpegts_subscription), MPEGTS_SELECT_CODEC, MPEGTS_CODEC_KLV_ASYNC, MPEGTS_UNIT_PES, 0, 0};
    mpegts_subscription tables = {sizeof(mpegts_subscription), MPEGTS_SELECT_PSI, 0, MPEGTS_UNIT_SECTIONS, 0, 0};
    uint32_t id;
    uint8_t buf[1316];
    size_t n;
    FILE *f;
    mpegts_report *report = NULL;
    mpegts_report_summary summary;
    uint32_t p, s;
    char message[256];
    uint8_t *all = NULL;
    size_t total = 0;

    layout();
    printf("ABI %u VERSION %s\n", mpegts_abi_version(), mpegts_version());
    if (mpegts_abi_version() != MPEGTS_ABI_VERSION || strcmp(mpegts_version(), MPEGTS_VERSION_STRING) != 0) {
        fprintf(stderr, "library and header disagree\n");
        return 1;
    }
    if (argc < 2) {
        return 0;
    }
    f = fopen(argv[1], "rb");
    if (!f) {
        perror(argv[1]);
        return 1;
    }
    config.struct_size = sizeof config;
    check("config_default", mpegts_config_default(&config));
    config.max_pes_bytes = 10;
    printf("CHECK %d %s\n", mpegts_config_check(&config, message, sizeof message), message);
    check("config_default", mpegts_config_default(&config));
    config.history_packets = 100;
    check("new", mpegts_demuxer_new(&config, &d));
    check("subscribe klv", mpegts_demuxer_subscribe(d, &klv, &id));
    printf("SUB klv=%u\n", id);
    check("subscribe tables", mpegts_demuxer_subscribe(d, &tables, &id));
    printf("SUB tables=%u\n", id);

    while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
        check("push", mpegts_demuxer_push(d, buf, n));
        drain(d);
        all = realloc(all, total + n);
        if (!all) {
            return 1;
        }
        memcpy(all + total, buf, n);
        total += n;
    }
    fclose(f);
    check("finish", mpegts_demuxer_finish(d));
    drain(d);

    check("report", mpegts_demuxer_report(d, &report));
    summary.struct_size = sizeof summary;
    check("summary", mpegts_report_summary_get(report, &summary));
    printf("SUMMARY tsid=%u programs=%u packets=%llu packet_size=%u duration_ns=%llu\n", summary.transport_stream_id,
           summary.program_count, (unsigned long long)summary.packets, summary.packet_size,
           (unsigned long long)summary.duration_ns);
    for (p = 0; p < summary.program_count; p++) {
        mpegts_program_info prog;
        prog.struct_size = sizeof prog;
        check("program", mpegts_report_program(report, p, &prog));
        printf("PROGRAM %u pmt=%u pcr=%u streams=%u\n", prog.number, prog.pmt_pid, prog.pcr_pid, prog.stream_count);
        for (s = 0; s < prog.stream_count; s++) {
            mpegts_stream_info info;
            info.struct_size = sizeof info;
            check("stream", mpegts_report_stream(report, p, s, &info));
            printf("STREAM pid=%u codec=%s kind=%s lang=%s packets=%llu\n", info.pid, mpegts_codec_name(info.codec),
                   mpegts_kind_name(info.kind), (info.flags & MPEGTS_STREAM_HAS_LANGUAGE) ? info.language : "-",
                   (unsigned long long)info.packets);
        }
    }
    if (mpegts_report_program(report, summary.program_count, NULL) != MPEGTS_ERR_RANGE) {
        fprintf(stderr, "expected a range error\n");
        return 1;
    }
    mpegts_report_free(report);
    mpegts_demuxer_free(d);

    check("analyze", mpegts_analyze(all, total, &report));
    summary.struct_size = sizeof summary;
    check("summary", mpegts_report_summary_get(report, &summary));
    printf("ANALYZE programs=%u packets=%llu\n", summary.program_count, (unsigned long long)summary.packets);
    mpegts_report_free(report);
    free(all);
    printf("DONE\n");
    return 0;
}
