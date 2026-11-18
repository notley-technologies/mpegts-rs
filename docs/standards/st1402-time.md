# Phase 0: ST 1402.2 carriage, MISP time (ST 0603.5) and video timestamps (ST 0604.6)

> Phase 0 record. Where this document and [DECISIONS.md](DECISIONS.md) disagree (sections 1 and 3 there), DECISIONS.md wins. Source paths `specs/src/...` refer to the workspace copy of the official documents, which are not redistributed here.

Topic `st1402-time`. Scope: every requirement of MISB ST 1402.2 that affects producing or modifying a transport stream (sync and async KLV carriage, PTS, PSI, PCR, rates, multiple metadata streams), the MISP Time System of MISB ST 0603.5 and the conversions a library must implement, and the ST 0604.6 timestamps inside H.264 / H.265 / MPEG-2 video, including how they relate to the PTS and to ST 0601 tag 2. Rules are numbered `R-ST1402TIME-NN`; decisions `D-NN`; unresolved items `U-NN`. Every quote is listed in `st1402-time.quotes.json` and was checked mechanically against the file (whitespace collapsed, dashes normalised, within 5 lines of the cited line).

## 1. Sources

All official texts below are in `specs/src/`. Text extractions were made with pypdf 4.3.1 (the same tool and `=== PAGE n ===` page markers as the existing files there; `pdftotext` is not installed). Extraction artefacts such as split words ("ins erted", "MPEG -2") are reproduced verbatim in quotes so they can be checked.

| Document | Revision, date | File | Where obtained |
|---|---|---|---|
| MISB ST 1402.2 MPEG-2 Transport Stream for Class 1/Class 2 Motion Imagery, Audio and Metadata | 1402.2, 27 October 2016 | `ST1402.2.pdf/.txt` | already present (Wayback capture of gwg.nga.mil, see specs/tables/misc.md S3). Figures 4 and 5 (PES header layouts) are images; read from the PDF pages 8 and 10. |
| MISB ST 0603.5 MISP Time System and Timestamps | 0603.5, 5 October 2017 | `ST0603.5.pdf/.txt` | already present (NSG S3 bucket via Wayback 20261008210411) |
| MISB ST 0604.6 Timestamps for Class 1/Class 2 Motion Imagery | 0604.6, 5 October 2017 (current: referenced by MISP-2023.2; the NSG registry bucket lists ST0604.6.pdf as its only ST 0604 file) | `ST0604.6.pdf/.txt` (new, 11 pages, sha256 `429e49facb7a65fcb7c1ba550a570162e5c485850d62dfc0defeca5fd85b5e40`) | `http://web.archive.org/web/20200111025230id_/https://gwg.nga.mil/misb/docs/standards/ST0604.6.pdf`. The direct gwg URL answers 403, nsgreg.nga.mil resets TLS, and every Wayback capture of the NSG S3 object `doc/Document/ST0604.6.pdf` (2024-2025) is a 403 page. |
| SMPTE RP 217-2001 Nonsynchronized Mapping of KLV Packets into MPEG-2 Systems Streams | approved 2001-09-18, stamped "ARCHIVED OCTOBER 31, 2007" | `SMPTE-RP217-2001.pdf/.txt` (new, sha256 `779efc4eccc39d9a44f5381c8291b2a1a2ca92fe30d96da7c9c8c41560058eb1`) | `http://web.archive.org/web/20260622172508id_/https://pub.smpte.org/pub/rp217/rp0217-2001_stable2007.pdf` (earlier sessions could not obtain it) |
| MISP-2025.1 Motion Imagery Handbook | 2025-06-11 | `MISP-2025.1-Motion-Imagery-Handbook.pdf/.txt` (new, sha256 `3dda522c6d0b005247b7b277d5eaa97d3fa11b73d43c6c23b39a0d99c3d5c33c`) | NSG S3 `doc/Document/MISP-2025.1_Motion_Imagery_Handbook.pdf` via Wayback 20261003134832. ST 0603.5 and ST 0601.19 defer to the Handbook for UTC conversion. (MISP-2020.1 Handbook was also read; its conversion table is the same.) |
| MISP-2023.2 Motion Imagery Standards Profile | March 2023 | `MISP-2023.2.pdf/.txt` (new, sha256 `be03299d7e97651fe095ccecd170742b6423cb70f8ac29f964e4847ad4cba451`) | NSG S3 `doc/Document/MISP-2023.2.pdf` via Wayback 20250202154832. Confirms current revisions (ST 0603.5, ST 0604.6, ST 1402.2) and adds profile-level time and carriage requirements. |
| MISB TRM 0909.4 Constructing a MISP Compliant File/Stream (informative) | 23 October 2014 | `TRM0909.4.pdf/.txt` (new, sha256 `cd93a78386117c74d441109a475f928c54bb3fcfd708db59a1558ede0f26379d`) | `http://web.archive.org/web/20161221041403id_/http://www.gwg.nga.mil/misb/docs/trm/TRM0909.4.pdf` |
| ITU-T H.264 Advanced video coding | 10/2016 (the edition ST 0604.6 cites) | `H.264-201610.pdf/.txt` (new, 804 pages, sha256 `9a081368e0a8e7fb2f8d27479b554c105d31dcbf5e3c47ba0fec3d1315136904`) | `https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-H.264-201610-S!!PDF-E&type=items` (free) |
| ITU-T H.265 High efficiency video coding | v4, 12/2016 (= ISO/IEC 23008-2:2017 cited by ST 0604.6) | `H.265-201612.pdf/.txt` (new, 664 pages, sha256 `4008bb6e9f6f4aa5ab6bb05e1dc81526800c2d3f1000fc5b2fa3dfcb3e7d4375`) | `https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-H.265-201612-S!!PDF-E&type=items` (free) |
| ITU-T H.222.0 = ISO/IEC 13818-1:2015 | 10/2014 | `H.222.0-201410.txt` | already present |
| MISB ST 0601.19, ST 0902.8, ST 0107.4 | as in specs/src | `ST0601.19.txt`, `ST0902.8.txt`, `ST0107.4.txt` | already present (only the time, rate and refresh rules are used here) |

Not obtained: ITU-T H.262 (MPEG-2 video; not free at ITU, the download returns an HTML page) and ISO/IEC 13818-4 (conformance). No ST 0604 revision newer than 0604.6 appears in MISP-2023.2, in the NSG registry bucket listing or on gwg.nga.mil captures.

Reference implementations used as cross-checks only (official text wins; disagreements are listed in section 5):

- FFmpeg muxer with the KLV series: `ffmpeg-klv-fix/ffmpeg/libavformat/mpegtsenc.c`, `ffmpeg-klv-fix/patches/0001-0004`; demuxer `specs/ref/ff_mpegts.c`.
- GStreamer: `specs/ref/tsmuxstream.c`, `specs/ref/gstbasetsmux.c`, demuxer `tsdemux.c` (GStreamer upstream).
- jMISB (github.com/WestRidgeSystems/jmisb): `api/.../st0603/TimeStatus.java`, `ST0603TimeStamp.java`; its README lists ST 0604 as "Partly implemented"; no SEI writer.
- KWIVER (fetched 2026-11-17 from github.com/Kitware/kwiver, branch main and release): `arrows/klv/misp_time.h`, `arrows/klv/misp_time.cxx`, `arrows/ffmpeg/algo/ffmpeg_video_input.cxx`. Note: The cbrl/klv library is not KWIVER; it is a header-only C++23 `klv` library (it implements ST 0603 conversions and the Time Status byte, not ST 0604).

## 2. Rules

### Document status

**R-ST1402TIME-01.** Build to MISB ST 1402.2 (carriage), ST 0603.5 (time) and ST 0604.6 (video timestamps): these are the revisions the current Motion Imagery Standards Profile (MISP-2023.2) references, and a TS carrying Class 1 Motion Imagery must comply with ST 1402.
- Implements: all
- Source: MISP-2023.2 references [12], [39], [46]; MISP-2015.1-48
- Quote: "[39] MISB ST 0604.6 Timestamps for Class 1/Class 2 Motion Imagery, Oct 2017." (`specs/src/MISP-2023.2.txt:2725`)
- Quote: "[12] MISB ST 0603.5 MISP Time System and Timestamps, Oct 2017." (`specs/src/MISP-2023.2.txt:2685`)
- Quote: "Class 1 Motion Imagery encapsulated in a MPEG-2 Transport Stream container shall comply with MISB ST 1402." (`specs/src/MISP-2023.2.txt:1420`)

**R-ST1402TIME-02.** ST 1402-05, -06, -19 and -23 are deprecated only because ISO/IEC 13818-1 already mandates them. Still write the ISO values: stream_type 0x15 with stream_id 0xFC for synchronous KLV, stream_type 0x06 with stream_id 0xBD for asynchronous KLV.
- Implements: mpegts::mux
- Source: ST 1402.2 section 3 (revision history) and the deprecated requirements table at the end of section 9.4.2
- Quote: "Deprecated REQ’s -05, -6, -19, -23; these are required by ISO/IEC 13818 -1 and should not be re -specified here." (`specs/src/ST1402.2.txt:24`)

### Transport stream: PSI repetition, PCR, null packets, UDP, PIDs

**R-ST1402TIME-03.** PAT and PMT each appear more than 4 times per second (every interval strictly below 250 ms); 8 per second (125 ms) is recommended. The injector emits its rewritten PAT and PMT on its own schedule (default every 100 ms, configurable, rejected if >= 250 ms) in addition to replacing the input's PSI, so the output conforms even when the input repeats PSI too rarely.
- Implements: mpegts::mux (ProgramTables, Injector); checker (max PAT and max PMT interval < 250 ms, warning above 125 ms)
- Source: ST 1402.2 section 7.1.2, ST 1402-02
- Quote: "Both a Program Association Table (PAT) and a Program Map Table (PMT) shall be inserted in the transport stream greater than 4 times per second (8 times per second recommended)" (`specs/src/ST1402.2.txt:144`)

**R-ST1402TIME-04.** A PCR appears on the program's PCR_PID at least every 100 ms, with at least two PCRs between PCR discontinuities. The injector never re-stamps, moves or drops a PCR, never changes PCR_PID, and its inserted KLV and PSI packets never carry a PCR. The later elementary-stream Muxer generates PCRs at <= 100 ms (default 40 ms).
- Implements: mpegts::mux (Injector, later Muxer); checker
- Source: ST 1402.2 section 7.2; ISO/IEC 13818-1 section 2.7.2
- Quote: "a Program Clock Reference (PCR) is inserted at least once every 100 milliseconds (as required by ISO/IEC 13818 -1)" (`specs/src/ST1402.2.txt:149`)
- Quote: "in successive occurrences of the PCRs in transport stream packets of the PCR_PID for each program shall be less than or equal to 0.1 s." (`specs/src/H.222.0-201410.txt:7082`)
- Quote: "There shall be at least two (2) PCRs, from the specified PCR_PID within a transport stream, between consecutive PCR discontinuities" (`specs/src/H.222.0-201410.txt:7087`)

**R-ST1402TIME-05.** Null packets (PID 0x1FFF) are the constant-bit-rate mechanism and receivers ignore them, so Cbr { replace_nulls } may overwrite null packets with KLV and PSI packets without changing the packet rate.
- Implements: mpegts::mux (Injector CBR mode)
- Source: ST 1402.2 section 7.5
- Quote: "Null packets , with a default PID of 0x1FFF, are used to form a constant bit rate stream. A Null packet is ignored by a receiver ." (`specs/src/ST1402.2.txt:203`)

**R-ST1402TIME-06.** Every UDP datagram carries a whole number of 188-byte TS packets (the number may vary per datagram); for a 1500-byte MTU send at most 7. The libraries hand out whole TS packets only; the klvmux CLI and any UDP sender pack 7 per datagram by default.
- Implements: mpegts::mux (sink granularity); klvmux CLI
- Source: ST 1402.2 section 8, ST 1402-01
- Quote: "All User Datagram Protocol (UDP) datagrams encapsulating MPEG -2 Transport Stream (TS) packets shall contain an integer number of TS packets. Each UDP datagram may contain a different integer number of TS packets." (`specs/src/ST1402.2.txt:216`)
- Quote: "network with a Maximum Transmission Unit (MTU) of 1500 bytes the recommended maximum number is seven (7) TS packets." (`specs/src/ST1402.2.txt:213`)

**R-ST1402TIME-07.** PIDs are not persistent across processing stages: the injector finds streams through PAT and PMT (never by fixed PID), follows PMT PID changes, and may choose the KLV PID itself (default: first free PID above the program's existing PIDs).
- Implements: mpegts::mux (Injector)
- Source: ST 1402.2 section 7.1.1 (informative)
- Quote: "These PIDs are generally assigned by the TS multiplexer , and, their values may be re -assigned through subsequent stages of processing i.e. not persistent ." (`specs/src/ST1402.2.txt:123`)

**R-ST1402TIME-08.** The TS must also meet the conformance requirements of ISO/IEC 13818-4, so the conformance checker covers timing and buffer constraints (T-STD), not only syntax. ISO/IEC 13818-4 itself is not in specs/src (see Unresolved).
- Implements: checker
- Source: MISP-2023.2 section 3.6.10.1, MISP-2015.1-50
- Quote: "Class 1 Motion Imagery encapsulated in a MPEG-2 Transport Stream container shall meet the conformance requirements of ISO/IEC 13818-4." (`specs/src/MISP-2023.2.txt:1430`)

### Existing video and audio: constraints the injector must preserve

**R-ST1402TIME-09.** Recommended: one PTS per video frame, and at frame rates below 10 fps a PTS on every frame. The injector never removes a video PTS or DTS; when it rewrites a video PES (ST 0604 SEI insertion) the PTS and DTS bytes stay identical. The checker warns when video access units lack a PTS, because sync KLV and the ST 0604 anchor depend on per-frame PTS.
- Implements: mpegts::mux; checker
- Source: ST 1402.2 section 7.3 (MISB usability recommendations)
- Quote: "That a PTS be ins erted for every Motion Imagery frame, so there is a one -to-one association of a PTS to each Motion Imagery frame" (`specs/src/ST1402.2.txt:164`)
- Quote: "At frame rates below 10 FPS a PTS should be present for every frame." (`specs/src/ST1402.2.txt:168`)

**R-ST1402TIME-10.** All bytes of an access unit arrive before its decode time (MPEG-2 video at most 1 s early, H.264 at most 10 s early). The injector must not delay video or audio bytes in delivery time: in VBR mode it only holds a frame's first TS packet while it synchronously inserts KLV within the same push call, in CBR mode it never moves a PCR-bearing packet.
- Implements: mpegts::mux (Injector); checker
- Source: ST 1402.2 section 7.4 (restating ISO/IEC 13818-1)
- Quote: "must occur in the stream prior to the PCR time at which th ey must be decoded" (`specs/src/ST1402.2.txt:174`)
- Quote: "Thus, for MPEG -2 Motion I magery each picture must occur less than 1 second ahe ad of its display time; for H.264 each picture must occur less than 10 seconds ahead of its display time." (`specs/src/ST1402.2.txt:186`)

**R-ST1402TIME-11.** Recommended: random_access_indicator = 1 on the TS packet that starts an H.264 random access point, elementary_stream_priority_indicator = 1 when an I or IDR access unit is present. When the injector regenerates the TS packets of a video PES (SEI insertion) it copies the adaptation field flags of the original first packet (random_access_indicator, elementary_stream_priority_indicator, discontinuity_indicator) and keeps every PCR on its original packet position.
- Implements: mpegts::mux (SEI insertion)
- Source: ST 1402.2 section 7.4 (usability recommendations)
- Quote: "A TS packet at the start of a H.264/AVC RAP (Random Access Point: IDR picture or I picture with recovery point SEI, SPS and PPS from which Motion Imagery decoding can begin successfully) should have random_access_indicator set to 1." (`specs/src/ST1402.2.txt:194`)
- Quote: "the elementary_stream_priority_indicator bit should be set whenever an access unit containing an I or IDR picture is p resent." (`specs/src/ST1402.2.txt:200`)

**R-ST1402TIME-12.** Audio PTS values in a PES stream are at most 0.7 s apart; this 0.7 s rule applies only to video and audio streams, not to metadata streams. The injector passes audio through untouched; the checker applies the 0.7 s check to video and audio PIDs only, never to KLV PIDs.
- Implements: mpegts::mux; checker
- Source: ST 1402.2 section 9.3; ISO/IEC 13818-1 section 2.7.4
- Quote: "successive PTS values must not differ by more than 0.7 seconds for each packetized elementary stream" (`specs/src/ST1402.2.txt:254`)
- Quote: "the maximum difference between coded presentation timestamps referring to each elementary video or audio stream is 0.7 s." (`specs/src/H.222.0-201410.txt:7099`)

**R-ST1402TIME-13.** Every AVC and every HEVC access unit carried in a TS contains an access unit delimiter, which is its first NAL unit. The SEI inserter uses the AUD to find access unit starts and inserts after it; access units without AUD are non-conformant input (the inserter falls back to first-VCL detection, the checker reports it).
- Implements: mpegts::mux (SEI insertion); checker
- Source: ISO/IEC 13818-1 sections 2.14.1 and 2.17.1
- Quote: "Each AVC access unit shall contain an access unit delimiter NAL Unit;" (`specs/src/H.222.0-201410.txt:8558`)
- Quote: "Each HEVC access unit shall contain an access unit delimiter NAL unit." (`specs/src/H.222.0-201410.txt:9476`)

**R-ST1402TIME-14.** PES_packet_length = 0 is allowed only for video in TS. When SEI insertion grows a video PES: keep 0 if it was 0; otherwise write the new length, or 0 if the new length exceeds 65535. KLV PES packets (sync and async) always carry a non-zero PES_packet_length.
- Implements: mpegts::mux
- Source: ISO/IEC 13818-1 section 2.4.3.7 (PES_packet_length semantics)
- Quote: "A value of 0 indicates that the PES packet length is neither specified nor bounded and is allowed only in PES packets whose payload consists of bytes from a video elementary stream contained in transport stream packets." (`specs/src/H.222.0-201410.txt:2587`)

### Synchronous KLV carriage (stream_type 0x15, stream_id 0xFC)

**R-ST1402TIME-15.** Synchronous KLV follows ISO/IEC 13818-1 section 2.12 in full: metadata AU wrapper, metadata_descriptor, metadata_STD_descriptor, T-STD buffer model.
- Implements: mpegts::mux
- Source: ST 1402.2 section 9.4.1, ST 1402-04
- Quote: "When inserting metadata using the Synchronous Metadata Multiplex Method , it shall be implemented in accordance with ISO13818 -1." (`specs/src/ST1402.2.txt:297`)

**R-ST1402TIME-16.** PMT stream_type = 0x15 and PES stream_id = 0xFC; with this pair the Metadata Access Unit Wrapper (AU cells) is mandatory.
- Implements: mpegts::mux
- Source: ST 1402.2 section 9.4.1; ST 1402-05 and -06 (deprecated, ISO still requires them); ISO/IEC 13818-1 section 2.12.4
- Quote: "Table -34 defines a stream_type = 0x15 for “ Metadata carried in PES p ackets ,” and Table 2 -22 defines a stream_id = 0xFC for “ metadata stream .”" (`specs/src/ST1402.2.txt:293`)
- Quote: "For the Synchronous Metadata Multiplex Method, t he stream_id shall be 0xFC," (`specs/src/ST1402.2.txt:373`)
- Quote: "For the Synchronous Metadata Multiplex Method, t he stream_type shall be 0x15," (`specs/src/ST1402.2.txt:376`)
- Quote: "When using PES packets with a stream_type of 0x15 and a stream_id of 0xFC to transport the metadata, a Metadata Access Unit Wrapper shall be used" (`specs/src/H.222.0-201410.txt:8185`)

**R-ST1402TIME-17.** Every sync PES carries a PTS with PTS_DTS_flags = '10' and no DTS (metadata is decoded instantaneously). So PES_header_data_length = 5 and the PES header is 14 bytes (ST 1402.2 Figure 4).
- Implements: mpegts::mux
- Source: ST 1402-07, ST 1402-08, ST 1402.2 section 9.4.1
- Quote: "each PES packet shall have a PTS to be used to synchronize the metadata with the Motion Imagery ." (`specs/src/ST1402.2.txt:299`)
- Quote: "the PTS_DTS_flags shall be set to ‘10’." (`specs/src/ST1402.2.txt:301`)
- Quote: "It is assumed that metadata is decoded instantaneously (i.e., there is no decoding time stamp (DTS) in this method)." (`specs/src/ST1402.2.txt:281`)

**R-ST1402TIME-18.** The first PES payload byte is the first byte of a Metadata_AU_cell, and the payload is a sequence of whole cells and nothing else.
- Implements: mpegts::mux
- Source: ST 1402-09; ISO/IEC 13818-1 section 2.12.4
- Quote: "the first PES packet data byte shall be the first byte of a Metadata Access Unit Cell." (`specs/src/ST1402.2.txt:303`)
- Quote: "In each PES packet that carries metadata, t he first PES_packet_data_byte shall be the first byte of a Metadata_AU_cell." (`specs/src/H.222.0-201410.txt:8189`)

**R-ST1402TIME-19.** The PES PTS applies to every access unit (every cell) in that PES, so only access units with the same PTS may share a PES.
- Implements: mpegts::mux; misb::mux
- Source: ST 1402-10
- Quote: "the PTS in the PES header shall apply to each Access Unit contained in the PES packet." (`specs/src/ST1402.2.txt:305`)

**R-ST1402TIME-20.** The PTS is the time the metadata access unit becomes relevant; the T-STD decodes it and removes it from B_n at that instant. For KLV that describes a video frame, the PTS is that frame's presentation time (see R-ST1402TIME-22 and D-6).
- Implements: misb::mux
- Source: ST 1402-11; ISO/IEC 13818-1 section 2.12.4
- Quote: "the PTS shall signal the time that the metadata Access Unit becomes relevant." (`specs/src/ST1402.2.txt:307`)
- Quote: "The PTS signals the time at which the metadata Access Units are decoded instantaneously and removed from buffer B n in the STD." (`specs/src/H.222.0-201410.txt:8191`)

**R-ST1402TIME-21.** Delay through the STD is at most 1 s: every byte of a sync KLV PES enters the TS no earlier than PTS - 1 s (STC time extrapolated from PCRs) and completely before the PTS. The injector's insertion point (the start of the matching video frame, D-7) is clamped so it is never earlier than PTS - 1 s; the checker verifies both bounds.
- Implements: mpegts::mux (Injector); misb::mux; checker
- Source: ST 1402-12
- Quote: "the delay of any data through the System Target Decoder buffers shall be less than or equal to one secon d." (`specs/src/ST1402.2.txt:309`)

**R-ST1402TIME-22.** Metadata sampled at the same instant as a frame carries the same PTS as the frame; otherwise the two PTS differ by the sampling offset, and receivers associate metadata with the frame whose PTS is closest. Per-frame synthesized KLV therefore gets exactly the frame's PTS.
- Implements: misb::mux
- Source: ST 1402.2 section 9.4.1; ISO/IEC 13818-1 section 2.12.2
- Quote: "If the PTS of the metadata is sampled at the same time as the PTS of a Motion Imagery frame, the metadata and Motion Imagery frame will have the same PTS; otherwise, they will differ by some time offset." (`specs/src/ST1402.2.txt:277`)
- Quote: "the translated 90 -kHz metadata time reference shall be matched with the picture or frame whose PTS value is closest to the translated value." (`specs/src/H.222.0-201410.txt:8122`)

**R-ST1402TIME-23.** If the input already carries synchronous metadata, new synchronous metadata is added to that existing metadata elementary stream (same PID) as an additional metadata service, not as a second 0x15 stream. ExistingKlv::Keep for sync therefore means merge (D-5); Replace and Fail stay available; a separate second 0x15 PID is non-conformant and may only exist as an explicit opt-in that the checker flags.
- Implements: mpegts::mux (Injector); checker
- Source: ST 1402-13
- Quote: "When inserting synchronous metadata into a transport stream which already carries synchronous metadata, new metadata shall be added to the existing synchronous m etadata elementary stream." (`specs/src/ST1402.2.txt:312`)

**R-ST1402TIME-24.** The sync metadata stream is its own entry in the PMT of the same program as the video it describes.
- Implements: mpegts::mux
- Source: ST 1402-14
- Quote: "the metadata elementary stream shall be defined in the PMT as a s eparate stream within the same p rogram as the Motion Imagery elementary stream." (`specs/src/ST1402.2.txt:315`)

**R-ST1402TIME-25.** One metadata_descriptor (tag 0x26) per metadata service, placed in the ES_info loop of the metadata stream (never in program_info).
- Implements: mpegts::mux
- Source: ST 1402-15, ST 1402-16; ISO/IEC 13818-1 section 2.6.60
- Quote: "the PMT shall contain a metadata_descriptor for each metadata service within the metadata stream." (`specs/src/ST1402.2.txt:321`)
- Quote: "the metadata_descriptor shall be within the descriptor loop for the metadata elementary stream." (`specs/src/ST1402.2.txt:324`)
- Quote: "In an MPEG -2 TS, the descriptor is included in the PMT in the descriptor loop for the elementary stream that carries the metadata service." (`specs/src/H.222.0-201410.txt:5651`)

**R-ST1402TIME-26.** Exactly one metadata_STD_descriptor (tag 0x27) per metadata stream, also when the stream carries several services.
- Implements: mpegts::mux
- Source: ST 1402-17
- Quote: "the PMT shall contain a single metadata_std_descriptor for the metadata stream." (`specs/src/ST1402.2.txt:327`)

**R-ST1402TIME-27.** metadata_format = 0xFF (format given by metadata_format_identifier) and metadata_format_identifier = 0x4B4C5641 ("KLVA").
- Implements: mpegts::mux
- Source: ST 1402.1-26; ISO/IEC 13818-1 Table 2-85
- Quote: "KLV metadata shall be identifi ed by t he registered metadata_format_identifier 0x4B4C5641 ("KLVA")." (`specs/src/ST1402.2.txt:329`)
- Quote: "The value 0xFF indicates that the format is signalled by the metadata_format_identifier field." (`specs/src/H.222.0-201410.txt:5609`)

**R-ST1402TIME-28.** metadata_application_format = 0x0100 (General), the only value allowed; 0x0101 to 0x0103 are deprecated and never written (accepted on read).
- Implements: mpegts::mux
- Source: ST 1402.2 Appendix B Table 4
- Quote: "0x0100 (Default value only allowed ) General 0x0101 (deprecated) Geographic Metadata 0x0102 (deprecated) Annotation Metadata 0x0103 (deprecated) Still Image on Demand" (`specs/src/ST1402.2.txt:446`)

**R-ST1402TIME-29.** metadata_descriptor values: descriptor_length 9, metadata_service_id 0x00 for the first service, decoder_config_flags '000', DSM-CC_flag '0', reserved '1111' (byte layout in section 3).
- Implements: mpegts::mux
- Source: ST 1402.2 Appendix B Table 2
- Quote: "descriptor_tag 0x26 (38) 8 descriptor_length 0x09 (9) 8 metadata_application_format 0x0100 - 0x0103 (see Table 4) 16 metadata_format 0xFF 8 metadata_format_identifier 0x4B4C5641 = “KLVA” 32" (`specs/src/ST1402.2.txt:403`)
- Quote: "metadata_service_id 0x00 8 decoder_config_flags ‘000’ 3 DSM -CC_flag ‘0’ 1 reserved ‘1111’ 4" (`specs/src/ST1402.2.txt:408`)

**R-ST1402TIME-30.** decoder_config_flags '000' means no decoder configuration is needed (KLV needs none): no decoder_config bytes in the descriptor and decoder_config_flag = 0 in every cell.
- Implements: mpegts::mux
- Source: ISO/IEC 13818-1 Table 2-88
- Quote: "000 No decoder configuration is needed." (`specs/src/H.222.0-201410.txt:5811`)

**R-ST1402TIME-31.** metadata_STD_descriptor: tag 0x27, length 9, three groups of reserved '11' plus a 22-bit field; metadata_input_leak_rate (units of 400 bit/s) and metadata_buffer_size (units of 1024 bytes) are chosen by the encoder; metadata_output_leak_rate is unspecified for 0x15 streams and set to 0. Chosen values: D-3.
- Implements: mpegts::mux
- Source: ST 1402.2 Appendix B Table 2 and note; ISO/IEC 13818-1 section 2.6.63
- Quote: "descriptor_tag 0x27 (39) 8 descriptor_length 0x09 (9) 8 reserved ‘11’ 2 metadata_input_leak_rate (determined by encoder) reserved ‘11’ 2 metadata_buffer_size (determined by encoder) reserved ‘11’ 2 metadata_output_leak_rate* (unspecified; recommend setting to 0)" (`specs/src/ST1402.2.txt:417`)
- Quote: "the metadata_output_leak_rate is unspecified for synchronous metadata. The recommended value is 0." (`specs/src/ST1402.2.txt:429`)
- Quote: "into buffer B n. The leak rate is specified in units of 400 bits/s." (`specs/src/H.222.0-201410.txt:5894`)
- Quote: "The size of B n is specified in units of 1024 bytes." (`specs/src/H.222.0-201410.txt:5898`)
- Quote: "For metadata stre ams transported synchronously (stream -type 0x15 or 0x19), the metadata access units are instantaneously removed from B n under the control of PTS timestamps and in that case the coding of the metadata_output_leak_rate field is not specified." (`specs/src/H.222.0-201410.txt:5900`)

**R-ST1402TIME-32.** AU cell header: metadata_service_id = the service's id; sequence_number + 1 (mod 256) for each successive cell of the metadata stream, independent of the service id; cell_fragment_indication per Table 2-109; decoder_config_flag 0; random_access_indicator 1 on a cell that starts a self-contained access unit (every complete ST 0601 packet, and the first fragment of one), 0 on middle and last fragments; reserved '1111'.
- Implements: mpegts::mux
- Source: ST 1402.2 Appendix B Table 2; ISO/IEC 13818-1 section 2.12.4.1
- Quote: "metadata_service_id 0x00 8 sequence_number (supplied by encoder; increments each cell) 8 cell_fragmentation_indication ‘11’, ‘10’, ‘01’ or ‘00’ 2 decoder_config_flag ‘0’ 1 random_access_indicator ‘0’ or ‘1’ 1 reserved ‘1111’ 4" (`specs/src/ST1402.2.txt:422`)
- Quote: "This number increments by one for each successive metadata_AU_cell constituting the metadata_AU_wrapper , independent of the coded value of the metadata_service_id." (`specs/src/H.222.0-201410.txt:8255`)
- Quote: "random_access_indicator : This 1 -bit field, when coded with the value '1', indicates that the metadata carried in this metadata_AU_cell represents an en try point to the metadata service where decoding is possible without information from previous metadata_AU_cells." (`specs/src/H.222.0-201410.txt:8267`)

**R-ST1402TIME-33.** An access unit that does not fit in one cell is fragmented over consecutive cells ('10', '00'..., '01'); when it does not fit in one PES, each PES carries one fragment cell, and every one of those PES packets carries the same PTS (R-ST1402TIME-17, -19).
- Implements: mpegts::mux
- Source: ISO/IEC 13818-1 sections 2.12.4 and 2.12.4.1
- Quote: "If a metadata Access Unit does not fit entirely into a metadata_ AU_cell, then the metadata Access Unit shall be fragmented into multiple metadata_AU_cells" (`specs/src/H.222.0-201410.txt:8206`)
- Quote: "A PES packet may contain a single metadata_AU_cell. This is useful i f a metadata Access Unit does not fit into a single PES packet" (`specs/src/H.222.0-201410.txt:8194`)

**R-ST1402TIME-34.** Default packing: one complete access unit (one ST 0601 packet) in one cell in one PES, the most common implementation.
- Implements: mpegts::mux; misb::mux
- Source: ST 1402.2 section 9.4.1 (informative)
- Quote: "In the most common implementations, the packet payload will consist of a single metadata Access Unit or Metadata_AU_cell" (`specs/src/ST1402.2.txt:283`)

**R-ST1402TIME-35.** metadata_service_id is unique among all metadata services of the whole transport stream (all programs), not only within one metadata stream.
- Implements: mpegts::mux (ProgramTables, Injector)
- Source: ST 1402.2 section 9.4.1; ISO/IEC 13818-1 sections 2.12.1 and 2.12.9.1
- Quote: "When transporting metadata using this service, a unique metadata_service_id is assigned to each service." (`specs/src/ST1402.2.txt:288`)
- Quote: "A metadata service id references uniquely a metadata service among all the metadata services available on the same transport or program stream" (`specs/src/H.222.0-201410.txt:8046`)

**R-ST1402TIME-36.** T-STD for metadata: TB_n is 512 bytes and drains at Rx_n = metadata_input_leak_rate; B_n has size metadata_buffer_size; a sync access unit leaves B_n at its PTS. TB_n must not overflow and must empty at least once per second; B_n must neither overflow nor underflow. The writer enforces the values it declares; the checker models them.
- Implements: mpegts::mux; checker
- Source: ISO/IEC 13818-1 sections 2.12.10 and 2.4.2
- Quote: "In the T -STD, rate Rx n is the rate out of TB n and equal to the rate defined by the metadata_input_leak_rate field in the metadata STD descriptor." (`specs/src/H.222.0-201410.txt:8518`)
- Quote: "The size BS n of buffer B n is equal to the size defined in the metadata_buffe r_size field in the metadata STD descriptor." (`specs/src/H.222.0-201410.txt:8520`)
- Quote: "At decode time, that is when the STC equals the PTS, the associated metadata is removed instantaneously from B n." (`specs/src/H.222.0-201410.txt:8521`)
- Quote: "Buffer B n shall not overflow." (`specs/src/H.222.0-201410.txt:8524`)
- Quote: "The transport buffer size is fixed at 512 bytes." (`specs/src/H.222.0-201410.txt:1469`)
- Quote: "TB n and TB sys shall not overflow. TB n and TB sys shall empty at least once every second. Bn shall not overflow nor underflow." (`specs/src/H.222.0-201410.txt:1668`)

**R-ST1402TIME-37.** The metadata content time line (tag 2 for KLV) has no discontinuities, is locked to the content's sampling clock, and is the same time line for every time reference in the stream. Tag 2 within one service follows PTS progression; never mix sources with different time lines in one service; the checker flags tag 2 jumps that the source's Time Status does not explain.
- Implements: misb::mux; checker
- Source: ISO/IEC 13818-1 section 2.12.2
- Quote: "no time discontinuities shall occur in the metadata content time line;" (`specs/src/H.222.0-201410.txt:8096`)
- Quote: "the metadata content time line shall be locked to th e sampling clock of the content;" (`specs/src/H.222.0-201410.txt:8097`)
- Quote: "each time reference in the metadata stream refers to the same metadata content time line." (`specs/src/H.222.0-201410.txt:8098`)

**R-ST1402TIME-38.** The metadata_pointer_descriptor (tag 0x25) is optional ("may"); if written, one per pointed-to service. ST 1402.2 never mentions it. Not written by default (D-2).
- Implements: mpegts::mux
- Source: ISO/IEC 13818-1 section 2.6.58
- Quote: "In a transport stream, the descriptor may be located in the PMT in the descriptor loop for either the program or an elementary stream" (`specs/src/H.222.0-201410.txt:5476`)
- Quote: "A unique metadata pointer descriptor shall be used to point to each metadata service used by the program or audiovisual stream." (`specs/src/H.222.0-201410.txt:5491`)

### Asynchronous KLV carriage (stream_type 0x06, stream_id 0xBD, SMPTE RP 217)

**R-ST1402TIME-39.** Asynchronous KLV follows SMPTE RP 217 (now in specs/src).
- Implements: mpegts::mux
- Source: ST 1402.2 section 9.4.2, ST 1402-18
- Quote: "When inserting metadata using the As ynchronous Metadata Multiplex Method (MISB ST 1402 ), it shall be implemented in accordance with SMPTE RP 217 ." (`specs/src/ST1402.2.txt:347`)

**R-ST1402TIME-40.** PMT stream_type = 0x06 and PES stream_id = 0xBD. A program may hold several async KLV streams, each on its own PID.
- Implements: mpegts::mux
- Source: ST 1402.2 section 9.4.2; ST 1402-19 and -23 (deprecated); RP 217 sections 4 and 4.1.1
- Quote: "Table -34 defines a stream_type = 0x06 for “PES packets containing p rivate data,” and Table 2 -22 defines a stream_id = 0xBD for “private_stream_1.”" (`specs/src/ST1402.2.txt:342`)
- Quote: "For the Asynchronous Metadata Multiplex Method, t he stream_id shall be 0xBD," (`specs/src/ST1402.2.txt:379`)
- Quote: "For the Asynchronous Metadata Multiplex Method, t he stream_type shall be 0x06," (`specs/src/ST1402.2.txt:382`)
- Quote: "The value of the ISO/IEC 13818-1 stream_id field associated with an SMPTE KLV program element shall be equal to 0xBD (private_stream_1)." (`specs/src/SMPTE-RP217-2001.txt:63`)
- Quote: "Multiple KLV streams may belong to an MPEG-2 program. For each, the stream_type field is set to PES private data (0x06)." (`specs/src/SMPTE-RP217-2001.txt:83`)

**R-ST1402TIME-41.** data_alignment_indicator = 1 exactly when the PES contains the beginning of a KLV item, else 0. With the RP 217 packing below this is 1 on every async PES except the continuation PES packets of a segmented item.
- Implements: mpegts::mux
- Source: ST 1402-20, ST 1402-21; RP 217 section 4.3
- Quote: "the data_alignment_indicator shall be set to one when the PES packet contains the beginning of a KLV item." (`specs/src/ST1402.2.txt:349`)
- Quote: "the data_alignment_indicator shall be set to zero when the PES packet does not contain the beginning of a KLV item." (`specs/src/ST1402.2.txt:356`)
- Quote: "The data_alignment_indicator shall be set to one when the PES packet contains the beginning of a KLV item, and shall be set to zero otherwise." (`specs/src/SMPTE-RP217-2001.txt:135`)

**R-ST1402TIME-42.** PTS_DTS_flags = '00' and ESCR_flag = 0, so PES_header_data_length = 0 and the header is 9 bytes (ST 1402.2 Figure 5).
- Implements: mpegts::mux
- Source: ST 1402-22; RP 217 section 4.2
- Quote: "the PTS_DTS_flags shall be set to 00 (no PTS or DTS present in PES packet header)." (`specs/src/ST1402.2.txt:359`)
- Quote: "The value of the PTS_DTS_flags shall be set to 00, and the ESCR_flag shall also be set to zero." (`specs/src/SMPTE-RP217-2001.txt:127`)

**R-ST1402TIME-43.** The async stream is an entry in the PMT of the same program as the video (and of exactly one program).
- Implements: mpegts::mux
- Source: ST 1402-24; RP 217 section 4.1.1
- Quote: "the metadata elementary stream shall be de fined in the PMT as a separate stream within the same p rogram as the Motion Imagery elementary stream." (`specs/src/ST1402.2.txt:361`)
- Quote: "each KLV encoded stream shall be part of an MPEG-2 program and hence shall be listed as an entry in one ISO/IEC 13818-1 program in the program map table (PMT) for that program." (`specs/src/SMPTE-RP217-2001.txt:79`)

**R-ST1402TIME-44.** A registration_descriptor (tag 0x05, length 4, format_identifier 0x4B4C5641 "KLVA") in the async stream's ES_info loop.
- Implements: mpegts::mux
- Source: ST 1402-25, ST 1402-03, ST 1402.2 Appendix B Table 3; RP 217 section 4.1.1
- Quote: "the program element loop in the PMT shall contain a registration_descriptor as defined in ISO/IEC 13818 -1 for legacy compliance with SMPTE RP 217." (`specs/src/ST1402.2.txt:364`)
- Quote: "KLV metadata shall be identified by t he registered format_identifier 0x4B4C5641 ("KLVA")." (`specs/src/ST1402.2.txt:367`)
- Quote: "descriptor_tag 0x05 (5) 8 descriptor_length 0x04 (4) 8 format_identifier 0x4B4C5641 = “KLVA” 32" (`specs/src/ST1402.2.txt:440`)
- Quote: "the program element loop in the PMT shall contain a registration_descriptor as defined in ISO/IEC 13818-1, and the format_identifier field shall be set to 0x4B4C5641 (KLVA)." (`specs/src/SMPTE-RP217-2001.txt:86`)

**R-ST1402TIME-45.** Async PES packing: every PES starts with the first byte of a KLV item; several whole items may share one PES; an item that fits in one PES never spans two; PES_packet_length is always > 0.
- Implements: mpegts::mux
- Source: RP 217 section 4.2
- Quote: "Each KLV item shall be placed into one or more PES packets. Multiple KLV items may be placed into a single PES packet as long as they fit wholly in a single PES packet." (`specs/src/SMPTE-RP217-2001.txt:110`)
- Quote: "The first KLV item in a PES packet shall be aligned with the beginning of the PES packet." (`specs/src/SMPTE-RP217-2001.txt:113`)
- Quote: "If KLV items are smaller than a PES packet, they shall not span multiple PES packets." (`specs/src/SMPTE-RP217-2001.txt:117`)
- Quote: "The value of the PES_packet_length field in the header of a PES packet conveying SMPTE KLV metadata shall be greater than zero." (`specs/src/SMPTE-RP217-2001.txt:124`)

**R-ST1402TIME-46.** An item larger than one PES (65535 minus header bytes) is split over consecutive PES packets of the same stream that carry only that item, with no PES of another item interleaved.
- Implements: mpegts::mux
- Source: RP 217 section 4.3
- Quote: "A single KLV item may span more than one PES packet. This shall be done for any KLV item that exceeds the maximum PES packet length (65535 minus headers)." (`specs/src/SMPTE-RP217-2001.txt:132`)
- Quote: "When a KLV item is segmented into more than one PES packet, all the PES packet segments of the item shall contain only the one KLV item." (`specs/src/SMPTE-RP217-2001.txt:141`)
- Quote: "When segmented KLV items are inserted into a single PES stream, the individual PES packets of different KLV items shall not be interleaved." (`specs/src/SMPTE-RP217-2001.txt:146`)

**R-ST1402TIME-47.** Async timing is by proximity only: insert async KLV at the stream position of the frame it describes (or at production time when live), and never add a PTS to it.
- Implements: misb::mux; mpegts::mux
- Source: ST 1402.2 section 9.4.2; RP 217 Annex B
- Quote: "The time relationship be tween a Motion Imagery frame and metadata is established by proximity to when metada ta is multiplexed into a TS , although this relationship cannot be guaranteed throughout decoding ." (`specs/src/ST1402.2.txt:335`)
- Quote: "Users are cautioned that private use of PTS or DTS may be incompatible with future applications or standards." (`specs/src/SMPTE-RP217-2001.txt:279`)

**R-ST1402TIME-48.** When one program carries both a sync and an async KLV stream, the ST 0102 security metadata (tag 48) goes into only one of them (configurable, default: the sync stream).
- Implements: misb::mux
- Source: MISP-2023.2 section 3.6.10.1, MISP-2015.1-49
- Quote: "Security metadata encapsulated in a MPEG-2 Transport Stream container shall be inserted into only one of the two carriage mechanisms available" (`specs/src/MISP-2023.2.txt:1426`)

### Metadata rates

**R-ST1402TIME-49.** ST 1402.2 sets no metadata rate. ST 0902 (informative): dynamic items as often as bandwidth allows up to the frame rate, static items (tags 3, 10, 11, 12, 48 and 94) once every 10 s. Default rate policy: sync KLV on every frame up to 30 Hz (every Nth frame above), never more often than the video frame rate.
- Implements: misb::mux
- Source: ST 0902.8 Annex A (informative)
- Quote: "Include Tags 3, 10, 11, 12, 48 sub-tags & 94 once every 10 seconds 2. Include all other items as often as possible, within the available bandwidth and up to the frame rate" (`specs/src/ST0902.8.txt:174`)
- Quote: "In some systems, it is desirable to have metadata updated for each Motion Imagery frame." (`specs/src/ST0902.8.txt:178`)

**R-ST1402TIME-50.** With report-on-change, every reported item is sent at least once per Metadata Refresh Period, whose maximum is 30 s. OnChange rate policies enforce a maximum interval <= 30 s (default 10 s for static items, matching R-ST1402TIME-49).
- Implements: misb::mux
- Source: ST 0107.4-14, ST 0107.4-18
- Quote: "Where a standard implements Report -on-Change for reportable items , Known - Changing or Known -Static items shall be reported no less than once every Metadata Refresh Period ." (`specs/src/ST0107.4.txt:262`)
- Quote: "the maximum value for the Metadata Refresh Period shall be 30 seconds." (`specs/src/ST0107.4.txt:271`)

### MISP time (ST 0603.5 and the Motion Imagery Handbook)

**R-ST1402TIME-51.** MISP Time System: epoch 1970-01-01T00:00:00.0Z, SI seconds, strictly monotonic (no skips, no repeats). MispTime is a plain linear count.
- Implements: misb::encode (MispTime)
- Source: ST 0603.5 section 6
- Quote: "Epoch of 1 970-01-01T00:00:00.0Z ( starting point for time scale )" (`specs/src/ST0603.5.txt:97`)
- Quote: "Base d on the SI Second (constant count ing increment of time )" (`specs/src/ST0603.5.txt:98`)
- Quote: "Strictly monotonically increasing (no skips, no repeats in count)" (`specs/src/ST0603.5.txt:99`)

**R-ST1402TIME-52.** MISP time = TAI - 8.000082 s (exactly), GPS = TAI - 19 s, UTC needs leap seconds. MISB notes many systems use 8 s; the library uses the exact 8.000082 s by default with an explicit legacy 8 s option (D-9).
- Implements: misb::encode
- Source: ST 0603.5 section 6 and Appendix A; Handbook 2025.1 section 6.2.3
- Quote: "fixed offset of 8.0000822 seconds between the MISP Time System and TAI (i.e. MISP Time = TAI - 8.000082 seconds) ." (`specs/src/ST0603.5.txt:101`)
- Quote: "(i.e. GPS Time = TAI - 19 seconds)" (`specs/src/ST0603.5.txt:104`)
- Quote: "UTC can be derived f rom the MISP Time System using its correct offset and inclusion of leap seco nds." (`specs/src/ST0603.5.txt:105`)
- Quote: "Many systems may not need to account for the 82 microseconds; those with sensitive timing requirements may." (`specs/src/ST0603.5.txt:118`)
- Quote: "To date, the offset used by MISB is 8 seconds (typical in common use), but the offse t is more accurately 8.0000 82." (`specs/src/ST0603.5.txt:309`)
- Quote: "The MISP Time System is locked with TAI with a Leap Second offset of 8.000082 seconds" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2536`)

**R-ST1402TIME-53.** Every Motion Imagery frame and every metadata packet with a time item carries an absolute timestamp, and one Motion Imagery instantiation uses only one representation (micro or nano). When adding or rewriting ST 0604 timestamps, put one in every access unit and keep the input's representation.
- Implements: misb::mux; mpegts::mux
- Source: ST 0603.5 section 7; MISP-2018.3-116, MISP-2018.1-98
- Quote: "It is mandatory that either a Precision Time Stamp or a Nano Precision Time Stamp be present in Motion Imagery and m etadata." (`specs/src/ST0603.5.txt:122`)
- Quote: "Every Motion Imagery frame shall include a timestamp representing Absolute Time consistent with MISB ST 0603." (`specs/src/MISP-2023.2.txt:755`)
- Quote: "An instantiation of Motion Imagery shall have only one timestamp representation which represents Absolute Time." (`specs/src/MISP-2023.2.txt:759`)

**R-ST1402TIME-54.** Precision Time Stamp: uint64 count of microseconds, sub-microsecond parts truncated (floor). Standalone KLV key 06.0E.2B.34.01.01.01.03.07.02.01.01.01.05.00.00.
- Implements: misb::encode
- Source: ST 0603.5 section 7.1, ST 0603.4-08
- Quote: "Represented as a 64-bit unsigned integer (UINT64) . 2. Specified to a resolution of 1 microsecond. Sub -microsecond measurements are truncated to the nearest microsecond." (`specs/src/ST0603.5.txt:132`)
- Quote: "The Precision Time Stamp shall represent a value of the MISP Time System truncated to the nearest microsecond ." (`specs/src/ST0603.5.txt:139`)
- Quote: "06.0E.2B.34.01.01.01.03.07.0 2.01.01.01.05.00.00 (CRC 64827)" (`specs/src/ST0603.5.txt:136`)

**R-ST1402TIME-55.** Nano Precision Time Stamp: uint64 count of nanoseconds, sub-nanosecond parts truncated. Standalone KLV key 06.0E.2B.34.01.01.01.01.0E.01.01.02.0A.08.00.00.
- Implements: misb::encode
- Source: ST 0603.5 section 7.2, ST 0603.5-09
- Quote: "The Nano Precision Time Stamp shall represent a value of the MISP Time System truncated to the nearest nanosecond ." (`specs/src/ST0603.5.txt:164`)
- Quote: "06.0E.2B.34.01.01.01.01.0E.01.01.02.0A.08 .00.00 (CRC 2123)" (`specs/src/ST0603.5.txt:161`)

**R-ST1402TIME-56.** Nano to micro: (ns + 500) / 1000 in integer arithmetic (rounds half up); micro to nano: x 1000. Test vectors: 31276 ns -> 31 us, 9572831 ns -> 9573 us, 9572632 ns -> 9573 us, 31 us -> 31000 ns, 9573 us -> 9573000 ns.
- Implements: misb::encode (tests)
- Source: ST 0603.5 section 7.3, Tables 1 and 2
- Quote: "Precision Time Stamp = (Nano Precision Time Stamp + 500)/1000" (`specs/src/ST0603.5.txt:178`)
- Quote: "0x00007A2C 31276 31776 31 0x0000001F 0x009211DF 9572831 9573331 9573 0x00002565 0x00921118 9572632 9573132 9573 0x00002565" (`specs/src/ST0603.5.txt:185`)
- Quote: "Nano Precision Time Stamp = 1000 * Precision Time Stamp" (`specs/src/ST0603.5.txt:195`)
- Quote: "0x0000001F 31 31000 0x00007918 0x00002565 9573 9573000 0x00921288" (`specs/src/ST0603.5.txt:202`)

**R-ST1402TIME-57.** Time Status byte: bit 7 = 1 lock unknown, bit 6 = 1 discontinuity, bit 5 = 1 reverse jump (meaningful only with bit 6), bits 4 to 0 reserved and written as '11111'. It travels with the timestamp inside the Motion Imagery (ST 0604).
- Implements: misb::encode
- Source: ST 0603.5 section 7.4, Table 3
- Quote: "bit 7 0 = Locked (interna l clock locked to absolute time reference ) 1 = Lock Unknown (internal clock not locked to absolute time reference )" (`specs/src/ST0603.5.txt:225`)
- Quote: "bit 6 0 = Normal (time increme nting forward in a linear fashion ) 1 = Discontinuity (time has not incremented forward in a linear fashion )" (`specs/src/ST0603.5.txt:227`)
- Quote: "bit 5 0 = Forward ( when bit 6 = 1 , indicates time jumped forward) 1 = Reverse ( when bit 6 = 1 , indicates time jumped backwards)" (`specs/src/ST0603.5.txt:229`)
- Quote: "bits 4 -0 Reserved (‘11111’)" (`specs/src/ST0603.5.txt:231`)
- Quote: "The Time Status is coded in Motion Imagery in conjunction with either the Precision Time Stamp , or the Nano Precision Time Stamp ." (`specs/src/ST0603.5.txt:237`)

**R-ST1402TIME-58.** Conversions (Handbook Table 17): MISP = TAI - MEpoch, MISP = GPS + GEpoch - MEpoch, MISP = ComputeTAI(UTC) - MEpoch, where TAI is counted from the TAI epoch 1958-01-01, MEpoch = 1970-01-01T00:00:08.000082 (TAI) and GEpoch = 1980-01-06T00:00:19 (TAI). Integer constants in section 3.10.
- Implements: misb::encode
- Source: Handbook 2025.1 section 6.2.5, Table 17
- Quote: "MISP (M) M + MEpoch ComputeUTC(M + MEpoch) M + MEpoch - GEpoch" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2606`)
- Quote: "UTC (U) ComputeTAI(U) ComputeTAI(U) - GEpoch ComputeTAI(U) - MEpoch" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2604`)
- Quote: "Where MEpoch and GEpoch are offsets to the TAI Epoch: MEpoch = MISPEpoch + 8.000082 GEpoch = GPSEpoch + 19" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2607`)

**R-ST1402TIME-59.** Bundle the TAI - UTC table of Handbook Table 14 (10 s from 1972-01-01 up to 37 s from 2017-01-01), dated and overridable; for 1970-01-01 to 1971-12-31 use Equation 2 with the last row of Table 15 (gives 8.000082 s at the epoch).
- Implements: misb::encode (LeapSeconds)
- Source: Handbook 2025.1 section 6.2.2.3, Tables 14 and 15, Example 1
- Quote: "2015-07-01T00:00:00 2017-01-01T00:00:00 36 1 leap second added 2017-01-01T00:00:00 Unknown 37 1 leap second added" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2411`)
- Quote: "1972-01-01T00:00:00 1972-07-01T00:00:00 10 Original offset from TAI" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2385`)
- Quote: "1968-02-01 1972-01-01 4.2131700 1966-01-01 0.002592" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2446`)
- Quote: "D = MJD(1970-01-01T00:00:00.0) = 40587.0 From last row in Table 15: B = 4.2131700 S = MJD(1966-01-01T00:00:00.0) = 39126.0 R = 0.002592" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2453`)

**R-ST1402TIME-60.** Around a leap second, MISP time keeps counting linearly while UTC shows 23:59:60 (inserted second) or skips 23:59:59 (removed second). from_utc accepts second = 60; a POSIX-style UTC count cannot express that second (D-10).
- Implements: misb::encode
- Source: Handbook 2025.1 section 6.2.5.2
- Quote: "Region B is a transition region with a duration of one SI-second. In this region the conversion must either include or exclude the UTC leap second" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2642`)

**R-ST1402TIME-61.** Reference vector: MISP 1657616732000618000 ns = TAI 2036307940000700000 ns (from 1958-01-01) = UTC 2022-07-12T09:05:03.000700Z with TAI - UTC = 37 s. As a Precision Time Stamp: 1657616732000618 us = 0x0005E397F87C116A.
- Implements: misb::encode (tests)
- Source: Handbook 2025.1 Appendix C.1.1
- Quote: "2022-07-12T09:05:32.000618000 (MISP) = 1657616732000618000 2022-07-12T09:05:40.000700000 (TAI) = 2036307940000700000 Leap second offset = 37 2022-07-12T09:05:03.000700000 (UTC)" (`specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:5676`)

**R-ST1402TIME-62.** POSIX or UTC counts are not MISP time. The deprecated ST 0603.2 rules (UTC without leap seconds, i.e. POSIX) are never the default; writing POSIX time needs the explicit opt-in MispTime::from_posix_assuming_no_offset.
- Implements: misb::encode
- Source: ST 0603.5 section 8 and Appendix A
- Quote: "UTC is only used to define the MISP Time System Epoch, not continuous time for implementations." (`specs/src/ST0603.5.txt:295`)
- Quote: "POSIX time does not include leap seconds; however, POSI X is not a linear scale of time." (`specs/src/ST0603.5.txt:314`)
- Quote: "The Precision Time Stamp shall be derived from Universal Coordinated Time (UTC) without leap seconds." (`specs/src/ST0603.5.txt:271`)

**R-ST1402TIME-63.** The Commercial Time Stamp (time code) is optional and, if used, follows SMPTE ST 12-1. We never generate one and never strip an existing one.
- Implements: misb::mux; mpegts::mux
- Source: ST 0603.5 section 7.5, ST 0603.2-07
- Quote: "When a Commercial Time Stamp is used, it shall be represented in accordance with SMPTE ST 12 -1 [5]." (`specs/src/ST0603.5.txt:249`)
- Quote: "A Commercial Time Stamp is optional in MISP Motion Imagery Systems." (`specs/src/ST0603.5.txt:252`)

### KLV tag 2, Precision Time Stamp (ST 0601.19)

**R-ST1402TIME-64.** Tag 2 is in every ST 0601 packet, as the first item, and holds the time of birth of all items in that packet.
- Implements: misb::encode
- Source: ST 0601.19 sections 6.3 and 6.4, ST 0601.8-09, ST 0601.8-10
- Quote: "All instances of a UAS Datalink LS shall contain Item 2, Precision Time Stamp, as the first item of the LS." (`specs/src/ST0601.19.txt:327`)
- Quote: "The value assigned to Item 2, Precision Time Stamp, shall represent the time of birth of the metadata of all the items contained in that instance of the UAS Datalink LS." (`specs/src/ST0601.19.txt:329`)

**R-ST1402TIME-65.** Tag 2 is not UTC (leap seconds for display come from Item 136 or a table). Never modify tag 2 of recorded packets: it breaks the frame correlation; post-hoc corrections go in Item 137. Re-stamping recorded tag 2 values is an explicit, logged option only.
- Implements: misb::mux; klvmux CLI
- Source: ST 0601.19 section 6.4
- Quote: "Note: this time does not include leap seconds and therefore the Precision Time Stamp does not represent UTC." (`specs/src/ST0601.19.txt:312`)
- Quote: "For the UTC conversion, the number of leap seconds may come from either the KLV packet (Item 136 - Leap Seconds), or from a current published leap second table." (`specs/src/ST0601.19.txt:315`)
- Quote: "Any modifications to the metadata Precision Time Stamp will break the synchronization between the metadata and Motion Imagery frames." (`specs/src/ST0601.19.txt:342`)

**R-ST1402TIME-66.** Frames and metadata are correlated through their Precision Time Stamps (ST 0604 in the frame, tag 2 in the packet). For synthesized metadata, tag 2 = the MISP time of the frame it is attached to, which equals that frame's ST 0604 time when present.
- Implements: misb::mux
- Source: ST 0601.19 sections 6.4 and 8.2.1
- Quote: "These Precision Time Stamps enable the correlation of Motion Imagery frames and the metadata." (`specs/src/ST0601.19.txt:341`)
- Quote: "The Precision Time Stamp is critical for synchronizing metadata to the Motion Imagery by correlating it to a Precision Time Stamp embedded in the Motion Imagery." (`specs/src/ST0601.19.txt:1389`)

### ST 0604.6 timestamps in the video

**R-ST1402TIME-67.** Timestamp source: a Precision (or Nano) Time Stamp pack in Class 0 imagery is extracted and used; otherwise an available absolute time reference generates the timestamp. For the injector, AddWhenMissing generates times from the anchor (input ST 0604 > explicit MISP or UTC anchor > live wall clock).
- Implements: misb::mux
- Source: ST 0604.5-16, ST 0604.6-20, ST 0604.6-21
- Quote: "When Class 0 Motion Imagery contains a Precision Time Stamp Pack, the Precision Time Stamp shall be extracted and used as the timestamp in the corresponding compressed Class 1/Class 2 Motion Imagery." (`specs/src/ST0604.6.txt:110`)
- Quote: "If Class 0 Motion Imagery does not contain a timestamp based on Absolute Time and one is available, then that time reference shall be used to generate either a Precision Time Stamp or a Nano Precision Time Stamp" (`specs/src/ST0604.6.txt:130`)

**R-ST1402TIME-68.** ST 0604 timestamp information = 16-byte identifier + 1-byte Time Status + 11-byte modified timestamp = 28 bytes.
- Implements: misb::encode (St0604 payload)
- Source: ST 0604.6 section 7, Figure 2
- Quote: "1. A Precision Time Stamp Identifier (16 bytes), 2. A Time Status (1 byte), and 3. A Modified Precision Time Stamp (11 bytes)." (`specs/src/ST0604.6.txt:142`)

**R-ST1402TIME-69.** H.262 and H.264 identifier: the ASCII bytes "MISPmicrosectime" = 4D 49 53 50 6D 69 63 72 6F 73 65 63 74 69 6D 65.
- Implements: misb::encode
- Source: ST 0604.6 section 7.1, Table 1
- Quote: "The Precision Time Stamp Identifier for H.262/MPEG-2 and H.264/AVC is the ASCII string “MISPmicrosectime”." (`specs/src/ST0604.6.txt:150`)
- Quote: "0x4D 0x49 0x53 0x50 0x6D 0x69 0x63 0x72" (`specs/src/ST0604.6.txt:156`)
- Quote: "0x6F 0x73 0x65 0x63 0x74 0x69 0x6D 0x65" (`specs/src/ST0604.6.txt:160`)

**R-ST1402TIME-70.** H.265 identifiers: microseconds UUID a8687dd4-d759-3758-a5ce-f0338b6545f1 (bytes A8 68 7D D4 D7 59 37 58 A5 CE F0 33 8B 65 45 F1), nanoseconds UUID cf848278-ee23-306c-9265-e8fef22fb8b8 (bytes CF 84 82 78 EE 23 30 6C 92 65 E8 FE F2 2F B8 B8). The identifier alone tells which resolution is carried; nanoseconds exist only for H.265.
- Implements: misb::encode
- Source: ST 0604.6 sections 5, 7.2, 8.1 and 12
- Quote: "The Precision Time Stamp Identifier for H.265/HEVC is the following UUID1: a8687dd4-d759-3758-a5ce-f0338b6545f1" (`specs/src/ST0604.6.txt:167`)
- Quote: "The Nano Precision Time Stamp Identifier for H.265/HEVC is the following UUID2: cf848278-ee23-306c-9265-e8fef22fb8b8" (`specs/src/ST0604.6.txt:215`)
- Quote: "UUID generated from the string “MISPmicrosectime-v2” as a version 3 UUID" (`specs/src/ST0604.6.txt:199`)
- Quote: "UUID generated from the string “MISPnanosectime-v1” as a version 3 UUID" (`specs/src/ST0604.6.txt:231`)
- Quote: "The selection between the Precision Time Stamp and the Nano Precision Time Stamp is signaled by the identifier used" (`specs/src/ST0604.6.txt:328`)
- Quote: "For H.265/HEVC [6] [7], there is a choice between either timestamp depending on application requirements." (`specs/src/ST0604.6.txt:85`)

**R-ST1402TIME-71.** The Time Status byte (ST 0603) precedes the modified timestamp, identically for every codec.
- Implements: misb::encode
- Source: ST 0604.6 sections 7.3 and 8.2
- Quote: "The Time Status is a one-byte value which precedes a Modified Precision Time Stamp" (`specs/src/ST0604.6.txt:172`)
- Quote: "The status byte is used for all compression types described here." (`specs/src/ST0604.6.txt:174`)

**R-ST1402TIME-72.** Modified timestamp: the 8-byte big-endian value written as 2 bytes, 0xFF, 2 bytes, 0xFF, 2 bytes, 0xFF, 2 bytes (payload bytes 18 to 28, byte 18 = most significant byte), for both micro and nano.
- Implements: misb::encode
- Source: ST 0604.6 section 7.4 Table 2, section 8.3 Table 3
- Quote: "The modified 8-byte Precision Time Stamp contains successive two-byte values spaced by a 0xFF value, thus forming an 11-byte Modified Precision Time Stamp value." (`specs/src/ST0604.6.txt:180`)
- Quote: "where Byte 18 is the most significant byte of the Precision Time Stamp." (`specs/src/ST0604.6.txt:183`)
- Quote: "Byte 17 Time Status (see MISB ST 0603 [1]) Bytes 18, 19 Byte 1 and 2 (Most significant bytes) of Precision Time Stamp" (`specs/src/ST0604.6.txt:189`)
- Quote: "Byte 20 Start Code Emulation Prevention Byte (0xFF) Bytes 21,22 Byte 3 and 4 of Precision Time Stamp Byte 23 Start Code Emulation Prevention Byte (0xFF)" (`specs/src/ST0604.6.txt:192`)
- Quote: "Byte 24, 25 Byte 5 and 6 of Precision Time Stamp" (`specs/src/ST0604.6.txt:195`)
- Quote: "Byte 26 Start Code Emulation Prevention Byte (0xFF) Byte 27, 28 Byte 7 and 8 (Least significant bytes) of Precision Time Stamp" (`specs/src/ST0604.6.txt:196`)
- Quote: "Along with the 16-byte Nano Precision Time Stamp Indicator, 28 bytes of data are then inserted into the compressed H.265 Motion Imagery." (`specs/src/ST0604.6.txt:240`)

**R-ST1402TIME-73.** H.264: user_data_unregistered SEI message, uuid_iso_iec_11578 = "MISPmicrosectime", user_data_payload_byte = Time Status then the 11-byte modified timestamp (byte 17 first).
- Implements: misb::encode; mpegts::mux (SEI NAL insertion)
- Source: ST 0604.6 section 11.1, ST 0604.4-10, ST 0604.4-12
- Quote: "The uuid_iso_iec_11578 is set to the 16-byte Precision Time Stamp Identifier. The user_data_payload_byte is set to the 12-byte combination Time Status and Modified Precision Time Stamp (Bytes 17-28 of Table 2 with Byte 17 transmitted first)." (`specs/src/ST0604.6.txt:298`)
- Quote: "A Precision Time Stamp Identifier, equal to the ASCII string “MISPmicrosectime,” shall be inserted into the H.264/AVC compressed user_data_unregistered SEI message field uuid_iso_iec_11578, so that it relates to a specific Motion Imagery frame." (`specs/src/ST0604.6.txt:303`)
- Quote: "A one-byte Time Status followed by an 11-byte Modified Precision Time Stamp shall be inserted into the H.264/AVC compressed user_data_unregistered SEI message field user_data_payload_byte, so that it relates to a specific Motion Imagery frame." (`specs/src/ST0604.6.txt:308`)

**R-ST1402TIME-74.** H.265: the same user_data_unregistered structure with the microseconds UUID (ST 0604.6-22, -23) or the nanoseconds UUID (ST 0604.6-24, -25).
- Implements: misb::encode; mpegts::mux
- Source: ST 0604.6 section 12
- Quote: "Where a Precision Time Stamp is used, a Precision Time Stamp Identifier equal to the UUID a8687dd4-d759-3758-a5ce-f0338b6545f1 shall be inserted into the H.265/HEVC compressed user_data_unregistered SEI message field uuid_iso_iec_11578," (`specs/src/ST0604.6.txt:343`)
- Quote: "Where a Precision Time Stamp is used, a one-byte Time Status followed by an 11- byte Modified Precision Time Stamp shall be inserted into the H.265/HEVC compressed user_data_unregistered SEI message field user_data_payload_byte," (`specs/src/ST0604.6.txt:347`)
- Quote: "Where a Nano Precision Time Stamp is used, a Nano Precision Time Stamp Identifier equal to the UUID cf848278-ee23-306c-9265-e8fef22fb8b8 shall be inserted into the H.265/HEVC compressed user_data_unregistered SEI message field uuid_iso_iec_11578," (`specs/src/ST0604.6.txt:362`)
- Quote: "Where a Nano Precision Time Stamp is used, a one-byte Times Status followed by an 11-byte Modified Nano Precision Time Stamp shall be inserted into the H.265/HEVC compressed user_data_unregistered SEI message field user_data_payload_byte," (`specs/src/ST0604.6.txt:366`)

**R-ST1402TIME-75.** H.262/MPEG-2: the 28 bytes go in a user_data field between the picture header and the picture data. Writing is out of scope for the first release (VideoTimestamps is rejected for MPEG-2 video); reading is supported.
- Implements: misb (decode); later mpegts::mux
- Source: ST 0604.6 section 10.1, ST 0604.5-18, ST 0604.5-19
- Quote: "A Precision Time Stamp Identifier, equal to the ASCII string “MISPmicrosectime,” shall be inserted into the H.262/MPEG-2 user_data field located between the picture header and the picture data." (`specs/src/ST0604.6.txt:278`)
- Quote: "A one-byte Time Status followed by an 11-byte Modified Precision Time Stamp shall be inserted into the H.262/MPEG-2 user_data field after the Precision Time Stamp Identifier," (`specs/src/ST0604.6.txt:281`)
- Quote: "which are inserted into the user_data field located between the picture header and the picture data" (`specs/src/ST0604.6.txt:270`)

**R-ST1402TIME-76.** Commercial time stamps (MPEG-2 GOP time_code, H.264 pic_timing, H.265 time_code SEI) are encoder requirements "where available". The injector never generates or strips them.
- Implements: mpegts::mux
- Source: ST 0604.5-17, ST 0604.3-09, ST 0604.3-11, ST 0604.4-15
- Quote: "Where Class 0 Motion Imagery contains a Commercial Time Stamp, the Commercial Time Stamp shall be extracted and used as the Commercial Time Stamp in the corresponding compressed Class 1/Class 2 Motion Imagery." (`specs/src/ST0604.6.txt:263`)
- Quote: "Where a Commercial Time Stamp is available, it shall be inserted into every Group- of-Pictures (GOP) of H.262/MPEG-2 compressed data." (`specs/src/ST0604.6.txt:291`)
- Quote: "Where a Commercial Time Stamp is available, it shall be inserted into the H.264/AVC pic_timing SEI message in every Class 1/ Class 2 Motion Imagery frame." (`specs/src/ST0604.6.txt:323`)
- Quote: "Where a Commercial Time Stamp is available, it shall be inserted into the H.265/HEVC time_code SEI message in every Class 1/Class 2 Motion Imagery frame." (`specs/src/ST0604.6.txt:377`)

**R-ST1402TIME-77.** Frequency: ST 0604.6 deprecated its own every-frame rule (ST 0604.3-07), but MISP-2018.3-116 still requires a timestamp in every frame (R-ST1402TIME-53). When enabled, insert in every access unit, IDR and non-IDR alike.
- Implements: mpegts::mux
- Source: ST 0604.6 section 13 (ST 0604.3-07 deprecated); MISP-2023.2
- Quote: "A Precision Time Stamp as defined in MISB ST 0603 shall be inserted in every Motion Imagery frame." (`specs/src/ST0604.6.txt:434`)

**R-ST1402TIME-78.** The 0xFF bytes are MISB's start code emulation prevention; they do not replace H.264/H.265 emulation prevention (R-ST1402TIME-81).
- Implements: mpegts::mux
- Source: ST 0604.6 section 6
- Quote: "measures are taken to prevent user-defined data to be mistakenly interpreted by a decoder - which is termed Start Code Emulation Prevention." (`specs/src/ST0604.6.txt:135`)

### H.264 and H.265 bitstream rules that apply when inserting the ST 0604 SEI

**R-ST1402TIME-79.** H.264 SEI NAL unit: forbidden_zero_bit 0, nal_ref_idc 0, nal_unit_type 6, so the NAL header byte is 0x06.
- Implements: mpegts::mux
- Source: ITU-T H.264 section 7.4.1, Table 7-1
- Quote: "nal_ref_idc shall be equal to 0 for all NAL units having nal_unit_type equal to 6, 9, 10, 11, or 12." (`specs/src/H.264-201610.txt:4139`)
- Quote: "6 Supplemental enhancement information (SEI) sei_rbsp( ) 5 non-VCL" (`specs/src/H.264-201610.txt:4197`)

**R-ST1402TIME-80.** SEI message coding: payloadType 5 and payloadSize 28 each fit in one byte (no ff_byte); payloadSize counts RBSP bytes, excluding emulation prevention bytes; user_data_unregistered is a 128-bit UUID followed by payload bytes; the SEI RBSP ends with rbsp_trailing_bits (0x80). One message per inserted NAL unit.
- Implements: mpegts::mux
- Source: ITU-T H.264 sections 7.3.2.3, 7.3.2.3.1, 7.4.2.3.1, D.1.1, D.1.7
- Quote: "sei_message( ) { C Descriptor payloadType = 0 while( next_bits( 8 ) = = 0xFF ) { ff_byte /* equal to 0xFF */ 5 f(8) payloadType += 255 }" (`specs/src/H.264-201610.txt:3403`)
- Quote: "last_payload_type_byte 5 u(8) payloadType += last_payload_type_byte payloadSize = 0" (`specs/src/H.264-201610.txt:3409`)
- Quote: "last_payload_size_byte 5 u(8) payloadSize += last_payload_size_byte" (`specs/src/H.264-201610.txt:3416`)
- Quote: "The derived SEI payload size payloadSize is specified in bytes and shall be equal to the number of RBSP bytes in the SEI payload." (`specs/src/H.264-201610.txt:5240`)
- Quote: "Since the payload size of an SEI message is specified in RBSP bytes, the quantity of emulation prevention bytes is not included in the size payloadSize of an SEI payload." (`specs/src/H.264-201610.txt:5243`)
- Quote: "user_data_unregistered( payloadSize ) { C Descriptor uuid_iso_iec_11578 5 u(128) for( i = 16; i < payloadSize; i++ ) user_data_payload_byte 5 b(8)" (`specs/src/H.264-201610.txt:17398`)
- Quote: "else if( payloadType = = 5 ) user_data_unregistered( payloadSize ) 5" (`specs/src/H.264-201610.txt:17163`)
- Quote: "sei_rbsp( ) { C Descriptor do sei_message( ) 5 while( more_rbsp_data( ) ) rbsp_trailing_bits( ) 5" (`specs/src/H.264-201610.txt:3394`)

**R-ST1402TIME-81.** Apply emulation prevention to the whole NAL unit after its header (insert 0x03 after two zero bytes when the next byte is <= 0x03). Never assume the ST 0604 0xFF bytes suffice: status 0x00 with time 0 needs it. Same rule in H.265.
- Implements: mpegts::mux
- Source: ITU-T H.264 section 7.4.1; ITU-T H.265 section 7.4.2
- Quote: "Within the NAL unit, the following three-byte sequences shall not occur at any byte-aligned position: - 0x000000 - 0x000001 - 0x000002" (`specs/src/H.264-201610.txt:4335`)
- Quote: "Within the NAL unit, any four-byte sequence that starts with 0x000003 other than the following sequences shall not occur at any byte-aligned position:" (`specs/src/H.264-201610.txt:4339`)
- Quote: "emulation_prevention_three_byte is a byte equal to 0x03. When an emulation_prevention_three_byte is present in the NAL unit, it shall be discarded by the decoding process." (`specs/src/H.264-201610.txt:4332`)
- Quote: "Within the NAL unit, the following three-byte sequences shall not occur at any byte-aligned position: - 0x000000 - 0x000001 - 0x000002" (`specs/src/H.265-201612.txt:3974`)

**R-ST1402TIME-82.** H.264 access unit order: the AUD is first; SEI NAL units precede the primary coded picture; a buffering period message must be the first message of the first SEI NAL unit. So the inserted SEI is a separate NAL unit placed after all existing non-VCL NAL units of the access unit, immediately before its first VCL NAL unit (nal_unit_type 1 to 5), or before the prefix NAL unit (type 14) that directly precedes it.
- Implements: mpegts::mux
- Source: ITU-T H.264 section 7.4.1.2.3
- Quote: "When an access unit delimiter NAL unit is present, it shall be the first NAL unit." (`specs/src/H.264-201610.txt:4514`)
- Quote: "When any SEI NAL units are present, they shall precede the primary coded picture." (`specs/src/H.264-201610.txt:4516`)
- Quote: "When an SEI NAL unit containing a buffering period SEI message is present, the buffering period SEI message shall be the first SEI message payload of the first SEI NAL unit in the access unit." (`specs/src/H.264-201610.txt:4517`)
- Quote: "NAL units having nal_unit_type equal to 0, 12, or in the range of 20 to 31, inclusive, shall not precede the first VCL NAL unit of the primary coded picture." (`specs/src/H.264-201610.txt:4529`)

**R-ST1402TIME-83.** In the byte stream format, the first NAL unit of an access unit (and SPS, PPS, VPS) needs the zero_byte. The inserted SEI is always written with the 4-byte start code 00 00 00 01, valid wherever it lands.
- Implements: mpegts::mux
- Source: ITU-T H.264 section B.1.2; ITU-T H.265 section B.2.2
- Quote: "When any of the following conditions are true, the zero_byte syntax element shall be present:" (`specs/src/H.264-201610.txt:16033`)
- Quote: "the byte stream NAL unit syntax structure contains the first NAL unit of an access unit in decoding order" (`specs/src/H.264-201610.txt:16035`)
- Quote: "When one or more of the following conditions are true, the zero_byte syntax element shall be present:" (`specs/src/H.265-201612.txt:13398`)
- Quote: "The byte stream NAL unit syntax structure contains the first NAL unit of an access unit in decoding order" (`specs/src/H.265-201612.txt:13400`)

**R-ST1402TIME-84.** H.265 SEI NAL unit: 2-byte header with forbidden_zero_bit 0, nal_unit_type 39 (PREFIX_SEI_NUT), nuh_layer_id 0, nuh_temporal_id_plus1 per R-ST1402TIME-85. user_data_unregistered is allowed in prefix and suffix SEI; we write prefix SEI.
- Implements: mpegts::mux
- Source: ITU-T H.265 section 7.3.1.2, Table 7-1, section D.2.1
- Quote: "nal_unit_header( ) { Descriptor forbidden_zero_bit f(1) nal_unit_type u(6) nuh_layer_id u(6) nuh_temporal_id_plus1 u(3)" (`specs/src/H.265-201612.txt:2435`)
- Quote: "39 40 PREFIX_SEI_NUT SUFFIX_SEI_NUT Supplemental enhancement information sei_rbsp( ) non-VCL" (`specs/src/H.265-201612.txt:4112`)
- Quote: "if( nal_unit_type = = PREFIX_SEI_NUT )" (`specs/src/H.265-201612.txt:14256`)
- Quote: "else if( payloadType = = 5 ) user_data_unregistered( payloadSize )" (`specs/src/H.265-201612.txt:14374`)

**R-ST1402TIME-85.** The TemporalId of an SEI NAL unit is >= the access unit's TemporalId, so copy nuh_temporal_id_plus1 from the access unit's first VCL NAL unit (header bytes 0x4E, 0x01 + TemporalId).
- Implements: mpegts::mux
- Source: ITU-T H.265 section 7.4.2.2
- Quote: "Otherwise, TemporalId shall be greater than or equal to the TemporalId of the access unit containing the NAL unit." (`specs/src/H.265-201612.txt:4200`)

**R-ST1402TIME-86.** H.265 order: prefix SEI NAL units never follow the last VCL NAL unit; suffix SEI never precede the first. Insert the prefix SEI immediately before the first VCL NAL unit of the access unit.
- Implements: mpegts::mux
- Source: ITU-T H.265 section 7.4.2.4.4
- Quote: "When any VPS NAL units, SPS NAL units, PPS NAL units, prefix SEI NAL units," (`specs/src/H.265-201612.txt:4366`)
- Quote: "are present, they shall not follow the last VCL NAL unit of the access unit." (`specs/src/H.265-201612.txt:4368`)
- Quote: "NAL units having nal_unit_type equal to FD_NUT or SUFFIX_SEI_NUT or in the range of RSV_NVCL45..RSV_NVCL47 or UNSPEC56..UNSPEC63 shall not precede the first VCL NAL unit of the access unit." (`specs/src/H.265-201612.txt:4369`)

**R-ST1402TIME-87.** H.265: an SEI NAL unit holding a non-nested buffering period, picture timing or decoding unit information message holds only those messages, and a buffering period message comes before other SEI messages. So our message goes in its own new SEI NAL unit placed after the existing prefix SEI NAL units.
- Implements: mpegts::mux
- Source: ITU-T H.265 section D.3.1
- Quote: "the SEI NAL unit shall not contain any other SEI message with payloadType not equal to 0 (buffering period), 1 (picture timing) or 130 (decoding unit information)." (`specs/src/H.265-201612.txt:15155`)
- Quote: "When a non-nested buffering period SEI message is present in an access unit, it shall not follow any other SEI message that follows the prevVclNalUnitInAu of the buffering period SEI message" (`specs/src/H.265-201612.txt:15168`)

**R-ST1402TIME-88.** uuid_iso_iec_11578 is meant to be an ISO/IEC 11578 UUID; MISB's H.264 identifier is the ASCII string instead. Writing follows MISB (ST 0604.4-10); readers accept all three identifiers in either codec.
- Implements: misb (decode, encode)
- Source: ITU-T H.264 section D.2.7; ITU-T H.265 section D.3.7
- Quote: "uuid_iso_iec_11578 shall have a value specified as a UUID according to the procedures of ISO/IEC 11578:1996 Annex A." (`specs/src/H.264-201610.txt:18145`)
- Quote: "uuid_iso_iec_11578 shall have a value specified as a UUID according to the procedures of Annex A of ISO/IEC 11578:1996." (`specs/src/H.265-201612.txt:15704`)

**R-ST1402TIME-89.** User data unregistered has no defined persistence (HEVC Table D.1), so the frame association comes only from being inside that access unit (ST 0604: "relates to a specific Motion Imagery frame"): at most one ST 0604 message per access unit, describing that access unit's picture.
- Implements: mpegts::mux; misb
- Source: ITU-T H.265 Table D.1 (informative)
- Quote: "User data unregistered Unspecified" (`specs/src/H.265-201612.txt:15093`)

### Relations between PTS, ST 0604 time and tag 2

**R-ST1402TIME-90.** The PTS lives only inside the TS and is discarded after demultiplexing; the Precision Time Stamp is absolute and independent of the PTS, though a PTS can be derived from absolute time (ST 0604). The anchor maps between the two; never write MISP time into a PTS field or the reverse.
- Implements: misb::mux
- Source: ST 1402.2 section 9.1
- Quote: "The PTS has no meaning outside of the TS, and it has no inherent relation to an absolute time syste m (although it can derive its value from an absolute time reference" (`specs/src/ST1402.2.txt:227`)
- Quote: "The Precision Time Stamp, on the other hand, represents the sampling of an absolute time reference, and is totally independent of the PTS." (`specs/src/ST1402.2.txt:231`)

**R-ST1402TIME-91.** MISP mandates a Precision Time Stamp in the video (ST 0604): the injector warns when the input video carries no ST 0604 timestamps and VideoTimestamps is Off.
- Implements: misb::mux; checker
- Source: ST 1402.2 section 9.2
- Quote: "the MISP mandates a Precision Time Stamp be inserted into Class 1/Class Motion Imagery; guidance for this is found in MISB ST 0604 [6]." (`specs/src/ST1402.2.txt:244`)

### Informative PES header values

**R-ST1402TIME-92.** Informative recommended PES header values: PES_priority 0, copyright 0, original_or_copy 1, no ESCR, ES_rate, trick mode, copy info, CRC or extension; sync PES with PTS only, async PES without PTS.
- Implements: mpegts::mux
- Source: TRM 0909.4 Appendix B Tables B3 and B4 (informative)
- Quote: "stream_id 8 0xFC (metadata stream) PES_packet_length 16 usage based ‘10’ 2 ‘10’ fixed PES_scrambling_control 2 ‘00’ (not scrambled) PES_priority 1 ‘0’ (low) data_alignment_indicator 1 usage based copyright 1 ‘0’ (not specified) original_or_copy 1 ‘1’ (original)" (`specs/src/TRM0909.4.txt:791`)
- Quote: "stream_id 8 0xBD (private stream 1) PES_packet_length 16 usage based ‘10’ 2 ‘10’ PES_scrambling_control 2 ‘00’ (not scrambled) PES_priority 1 ‘0’ (low) data_alignment_indicator 1 usage based copyright 1 ‘0’ (not specified) original_or_copy 1 ‘1’ (original)" (`specs/src/TRM0909.4.txt:769`)

## 3. Exact byte layouts

Bit numbering: bit 7 is the most significant bit of a byte. All multi-byte integers are big-endian. Values in `this font` are the ones we write.

### 3.1 Synchronous KLV PES header (14 bytes; ST 1402.2 Figure 4, R-ST1402TIME-17, -92, D-8)

| Byte | Bits | Field | Value |
|---|---|---|---|
| 0-2 | 24 | packet_start_code_prefix | `00 00 01` |
| 3 | 8 | stream_id | `FC` (metadata stream) |
| 4-5 | 16 | PES_packet_length | 8 + PES payload length (never 0; max 65535) |
| 6 | 7-6 | '10' | `10` |
| 6 | 5-4 | PES_scrambling_control | `00` |
| 6 | 3 | PES_priority | `0` |
| 6 | 2 | data_alignment_indicator | `1` (D-8) |
| 6 | 1 | copyright | `0` |
| 6 | 0 | original_or_copy | `1` |
| 7 | 7-6 | PTS_DTS_flags | `10` |
| 7 | 5-0 | ESCR, ES_rate, DSM_trick_mode, additional_copy_info, PES_CRC, PES_extension flags | `000000` |
| 8 | 8 | PES_header_data_length | `05` |
| 9 | 8 | '0010', PTS[32..30], marker '1' | |
| 10-11 | 16 | PTS[29..15], marker '1' | |
| 12-13 | 16 | PTS[14..0], marker '1' | |

So bytes 6-8 are `85 80 05`. Example, PTS = 900000 (10 s): `00 00 01 FC LL LL 85 80 05 21 00 37 77 41`. Maximum payload per PES: 65535 - 8 = 65527 bytes, so a single cell carries at most 65527 - 5 = 65522 bytes of access unit data.

### 3.2 Metadata_AU_cell header (5 bytes; ISO/IEC 13818-1 Table 2-108, ST 1402.2 Appendix B, R-ST1402TIME-32)

| Byte | Bits | Field | Value |
|---|---|---|---|
| 0 | 8 | metadata_service_id | the service's id (`00` for the first service, D-5) |
| 1 | 8 | sequence_number | previous cell of this PID + 1, mod 256 (first cell: 0) |
| 2 | 7-6 | cell_fragment_indication | `11` complete AU, `10` first, `00` middle, `01` last fragment |
| 2 | 5 | decoder_config_flag | `0` |
| 2 | 4 | random_access_indicator | `1` for `11` and `10`, `0` for `00` and `01` |
| 2 | 3-0 | reserved | `1111` |
| 3-4 | 16 | AU_cell_data_length | number of following bytes (<= 65522 with one cell per PES) |

Byte 2 values: complete `DF`, first fragment `9F`, middle `0F`, last `4F`.

### 3.3 metadata_descriptor (11 bytes; ISO/IEC 13818-1 Table 2-87, ST 1402.2 Appendix B, R-ST1402TIME-25, -27..-30)

| Byte | Bits | Field | Value |
|---|---|---|---|
| 0 | 8 | descriptor_tag | `26` |
| 1 | 8 | descriptor_length | `09` |
| 2-3 | 16 | metadata_application_format | `01 00` (General) |
| 4 | 8 | metadata_format | `FF` |
| 5-8 | 32 | metadata_format_identifier | `4B 4C 56 41` ("KLVA") |
| 9 | 8 | metadata_service_id | `00` (first service) |
| 10 | 7-5 | decoder_config_flags | `000` |
| 10 | 4 | DSM-CC_flag | `0` |
| 10 | 3-0 | reserved | `1111` |

Bytes: `26 09 01 00 FF 4B 4C 56 41 00 0F` (identical to what the patched FFmpeg writes).

### 3.4 metadata_STD_descriptor (11 bytes; ISO/IEC 13818-1 Table 2-89, ST 1402.2 Appendix B, R-ST1402TIME-26, -31, D-3)

| Byte | Bits | Field | Value |
|---|---|---|---|
| 0 | 8 | descriptor_tag | `27` |
| 1 | 8 | descriptor_length | `09` |
| 2 | 7-6 | reserved | `11` |
| 2-4 | 22 | metadata_input_leak_rate (x 400 bit/s) | default `3FFFFF` |
| 5 | 7-6 | reserved | `11` |
| 5-7 | 22 | metadata_buffer_size (x 1024 bytes) | default `000400` (1 MiB) |
| 8 | 7-6 | reserved | `11` |
| 8-10 | 22 | metadata_output_leak_rate (x 400 bit/s) | `000000` |

Default bytes: `27 09 FF FF FF C0 04 00 C0 00 00`.

### 3.5 PMT ES entries

Sync KLV: `15 | 111 + elementary_PID (13) | 1111 + ES_info_length (12) = 22 | metadata_descriptor (11) | metadata_STD_descriptor (11)`, e.g. PID 0x0102: `15 E1 02 F0 16 26 09 01 00 FF 4B 4C 56 41 00 0F 27 09 FF FF FF C0 04 00 C0 00 00`. With the optional legacy registration descriptor (D-1) ES_info_length becomes 28 and `05 04 4B 4C 56 41` is appended. One additional metadata_descriptor (11 bytes) per extra service on the same PID (R-ST1402TIME-23, -25).

Async KLV: `06 | 111 + PID | 1111 + ES_info_length = 6 | 05 04 4B 4C 56 41`, e.g. PID 0x0103: `06 E1 03 F0 06 05 04 4B 4C 56 41` (R-ST1402TIME-40, -44).

Optional metadata_pointer_descriptor (program_info loop, off by default, D-2): `25 0B 01 00 FF 4B 4C 56 41 <service_id> 1F <program_number:16>`; the `1F` byte is metadata_locator_record_flag 0, MPEG_carriage_flags `00` (same TS), reserved `11111`.

### 3.6 Asynchronous KLV PES header (9 bytes; ST 1402.2 Figure 5, R-ST1402TIME-41, -42, -45)

`00 00 01 BD LL LL F6 00 00` where LL LL = 3 + payload length (never 0, max 65535, so <= 65532 payload bytes) and F6 = `85` when the PES begins a KLV item (data_alignment_indicator 1), `81` for a continuation PES of a segmented item. Byte 7 `00` (PTS_DTS_flags `00`, ESCR_flag 0), byte 8 `00` (PES_header_data_length 0).

### 3.7 Time Status byte (ST 0603.5 Table 3, R-ST1402TIME-57)

| Bit | Meaning |
|---|---|
| 7 | 0 locked to absolute time reference, 1 lock unknown |
| 6 | 0 normal (linear), 1 discontinuity |
| 5 | when bit 6 = 1: 0 jumped forward, 1 jumped backward (write 0 when bit 6 = 0) |
| 4-0 | reserved, write `11111` |

Valid encoder values: `1F` locked, `9F` lock unknown, `5F` locked + forward jump, `7F` locked + backward jump, `DF` unknown + forward jump, `FF` unknown + backward jump.

### 3.8 ST 0604 timestamp information (28 bytes; ST 0604.6 Tables 1-3, R-ST1402TIME-68..-74)

| Payload byte (ST 0604 numbering) | Content |
|---|---|
| 1-16 | identifier: H.262/H.264 `4D 49 53 50 6D 69 63 72 6F 73 65 63 74 69 6D 65` ("MISPmicrosectime"); H.265 microseconds `A8 68 7D D4 D7 59 37 58 A5 CE F0 33 8B 65 45 F1`; H.265 nanoseconds `CF 84 82 78 EE 23 30 6C 92 65 E8 FE F2 2F B8 B8` |
| 17 | Time Status (3.7) |
| 18-19 | timestamp bytes 1-2 (most significant) |
| 20 | `FF` |
| 21-22 | timestamp bytes 3-4 |
| 23 | `FF` |
| 24-25 | timestamp bytes 5-6 |
| 26 | `FF` |
| 27-28 | timestamp bytes 7-8 (least significant) |

The timestamp is the uint64 Precision Time Stamp (microseconds) for "MISPmicrosectime" and the H.265 microseconds UUID, and the uint64 Nano Precision Time Stamp for the H.265 nanoseconds UUID.

### 3.9 Complete SEI NAL units (byte stream form)

Example values: Precision Time Stamp 1657616732000618 us = `00 05 E3 97 F8 7C 11 6A` (the Handbook vector, R-ST1402TIME-61), Time Status `9F`.

H.264 (36 bytes; R-ST1402TIME-73, -79..-83):

```
00 00 00 01                                       start code with zero_byte
06                                                forbidden 0, nal_ref_idc 0, nal_unit_type 6
05                                                last_payload_type_byte: user_data_unregistered
1C                                                last_payload_size_byte: 28 RBSP bytes
4D 49 53 50 6D 69 63 72 6F 73 65 63 74 69 6D 65   uuid_iso_iec_11578 = "MISPmicrosectime"
9F                                                Time Status
00 05 FF E3 97 FF F8 7C FF 11 6A                  modified Precision Time Stamp
80                                                rbsp_trailing_bits
```

H.265, microseconds (37 bytes; R-ST1402TIME-74, -84..-87): `00 00 00 01 4E 01 05 1C A8 68 7D D4 D7 59 37 58 A5 CE F0 33 8B 65 45 F1 9F 00 05 FF E3 97 FF F8 7C FF 11 6A 80`. The header bytes `4E 01` are nal_unit_type 39, nuh_layer_id 0, nuh_temporal_id_plus1 1; for an access unit with TemporalId t the second byte is `01 + t`.

H.265, nanoseconds, same instant (1657616732000618000 ns = `17 01 09 A2 A4 A4 06 10`): `00 00 00 01 4E 01 05 1C CF 84 82 78 EE 23 30 6C 92 65 E8 FE F2 2F B8 B8 9F 17 01 FF 09 A2 FF A4 A4 FF 06 10 80`.

Emulation prevention is applied to everything after the NAL header (R-ST1402TIME-81). With a non-zero status byte (always the case when the reserved bits are written as 1) no forbidden pattern can occur, because every other 3-byte window of the payload contains a 0xFF byte or a non-zero identifier byte; it must still run, because a status byte of 0x00 can produce one: status `00` with timestamp 0 gives the RBSP `... 00 00 00 FF ...`, which becomes `... 00 00 03 00 FF ...` in the NAL unit; payloadSize stays 28.

MPEG-2 video (not written in the first release, R-ST1402TIME-75): a user_data structure after the picture header, `00 00 01 B2` followed by the 28 bytes. The user_data_start_code value 0x000001B2 comes from H.262, which is not in specs/src (U-2).

### 3.10 KLV tag 2 and time constants

Tag 2 item in an ST 0601 local set: `02 08 <uint64 Precision Time Stamp>`, e.g. `02 08 00 05 E3 97 F8 7C 11 6A`.

Integer constants for the conversions (derived from Handbook Table 17 and checked against its Appendix C vector; units are microseconds unless noted):

- MEpoch = 1970-01-01T00:00:08.000082 TAI measured from 1958-01-01T00:00:00 TAI = 4383 days + 8.000082 s = `378_691_208_000_082` us (`378691208000082000` ns, printed in the Handbook Appendix C).
- GEpoch = 1980-01-06T00:00:19 TAI from the same origin = 8040 days + 19 s = `694_656_019_000_000` us.
- MISP = TAI_1958 - 378_691_208_000_082.
- MISP = GPS + 315_964_810_999_918, GPS counted from 1980-01-06T00:00:00 (GPS scale).
- MISP = U + (TAI - UTC)(U) x 1_000_000 - 8_000_082, where U is the UTC instant as a POSIX-style count (86400 s per day since 1970-01-01, no leap seconds) and (TAI - UTC) comes from Table 14 for U >= 1972-01-01. Since 2017-01-01: MISP = U + 28_999_918. Check: U = 2022-07-12T09:05:03.000700Z = 1657616703000700 gives 1657616732000618 (Handbook vector).
- For 1970-01-01 <= U < 1972-01-01: TAI - UTC = 4.2131700 + (MJD - 39126) x 0.002592 s (Equation 2, last row of Table 15), 8.000082 s at MJD 40587.
- UTC from MISP: U = MISP - (TAI - UTC) x 1_000_000 + 8_000_082, with the leap second zone looked up on the TAI date (Handbook 6.2.5.2); an inserted leap second maps to second 60 and has no POSIX representation.
- Nano: same formulas with every constant x 1000; Precision from Nano = (ns + 500) / 1000.

## 4. Decisions

**D-1 Sync PMT descriptors.** Write exactly: metadata_descriptor (3.3) then metadata_STD_descriptor (3.4) in the KLV stream's ES_info loop (R-ST1402TIME-25, -26). ST 1402.2 is silent on a registration_descriptor for sync streams ("The PMT may contain other descriptors", section 9.4.1). Default: no registration descriptor, because the format is already identified by metadata_format_identifier (ST 1402.1-26) and a "KLVA" registration descriptor is RP 217's async signalling. Option `sync_registration_descriptor = true` adds `05 04 4B 4C 56 41` for legacy receivers. Reference behaviour: the patched FFmpeg writes registration + metadata_descriptor and no metadata_STD_descriptor; FFmpeg's and GStreamer's demuxers both recognise KLV from the metadata_descriptor alone (`METADATA_types` in ff_mpegts.c, `GST_MTS_DESC_METADATA` branch in tsdemux.c), so the default interoperates.

**D-2 metadata_pointer_descriptor.** Not required: ISO says it "may" be placed (R-ST1402TIME-38) and ST 1402.2 never mentions it; ST 1402-14 already ties the metadata to the video by putting both in one program. Not written by default (FFmpeg and GStreamer never write it either). Option to write one in the program_info loop per service, layout in 3.5.

**D-3 metadata_STD_descriptor values.** Standard silent ("determined by encoder"). The binding constraints are ISO's T-STD rules (R-ST1402TIME-36): TB_n is only 512 bytes, so a burst of three or more back-to-back KLV TS packets overflows it unless Rx_n is close to the instantaneous transport rate, which a pass-through injector neither knows in advance nor controls (VBR input). Defaults:
- metadata_input_leak_rate = `0x3FFFFF` (1,677,721,200 bit/s, the maximum): TB_n then never holds more than a few bytes at any realistic transport rate, so it cannot overflow and empties within microseconds.
- metadata_buffer_size = `0x000400` (1 MiB). B_n holds each PES from arrival until its PTS, and ST 1402-12 caps that at 1 s, so 1 MiB covers about 1 MB/s of KLV (for example 30 Hz VMTI packets of 30 KB). The writer tracks B_n occupancy (PES bytes delivered, removed at PTS) and applies the configured overflow policy (fail, or drop with a counter) rather than exceed the declared size; the checker verifies.
- metadata_output_leak_rate = 0 (ST 1402.2 recommendation; ISO leaves it unspecified for 0x15).
Both encoder-chosen values are configurable; a configured lower leak rate makes the writer space the KLV TS packets so that TB_n stays <= 512 bytes. No reference implementation writes this descriptor (FFmpeg, GStreamer, jMISB via FFmpeg, KWIVER via FFmpeg), so there is no prior art to align with.

**D-4 What is a metadata access unit for KLV.** ISO leaves it format-specific and ST 1402.2 does not define it. Decision: one access unit = one complete KLV packet (one ST 0601 local set with its 16-byte key), carried as one cell in one PES (R-ST1402TIME-34). An access unit longer than 65522 bytes is fragmented, one fragment cell per PES, every PES with the same PTS (R-ST1402TIME-33); readers stay tolerant (several cells and several KLV packets per access unit are accepted). Option: several services' access units that share a PTS may share one PES.

**D-5 metadata_service_id and merging into an existing sync stream.** Our first service uses id 0x00 when no other service in the whole TS uses it, otherwise the lowest unused id (R-ST1402TIME-35). With ExistingKlv::Keep and an input that already has a 0x15 KLV stream, ST 1402-13 requires merging: our service gets a new id and its own metadata_descriptor on the existing PID's ES_info loop (one metadata_STD_descriptor stays, values unchanged unless our extra load needs a larger buffer), our PES packets go on that PID between complete input PES packets (a metadata PES always has a non-zero PES_packet_length, so its end is known), the PID's continuity counters are renumbered, and the cell sequence_number of every cell on that PID is renumbered because ISO counts cells per stream independently of the service id (R-ST1402TIME-32, U-5). Access unit bytes of the input are never changed. This is the only case where a pass-through PID is rewritten; the invariance test excludes it explicitly. ExistingKlv::Replace removes the input service (or the whole PID when it is the only service); Fail stops with a clear error; a second 0x15 PID is an opt-in that the checker reports as an ST 1402-13 violation. Async streams may simply be added on a new PID (RP 217 allows several).

**D-6 Sync PTS and tag 2.** For samples synthesized at a frame (Track, live state): PTS = that frame's PTS and tag 2 = MISP time of that frame (the anchor applied to the frame PTS, equal to the frame's ST 0604 time when the input has one), so PTS and time agree exactly (R-ST1402TIME-20, -22, -66). For recorded packets (Explicit source, `.klv`): tag 2 is never modified (R-ST1402TIME-65) and PTS = anchor^-1(tag 2), rounded to the nearest 90 kHz tick, not snapped (ST 1402.2 section 9.4.1 allows an offset to the frame PTS); an option snaps to the nearest frame PTS, mirroring the receiver rule in ISO 2.12.2.

**D-7 Insertion position.** Sync: immediately before the first TS packet of the video PES whose PTS equals the KLV PTS (or, without an exact match, the first video PES with PTS >= KLV PTS), but never earlier than the position whose STC (PCR-extrapolated) is PTS - 1 s (ST 1402-12); if the video frame itself arrives later than that window allows, insert at the video position anyway (still before the PTS) and report it. Async: immediately before the first TS packet of the described frame's video PES, or at production time in live mode (R-ST1402TIME-47).

**D-8 Remaining PES header bits.** ISO and ST 1402.2 are silent on data_alignment_indicator for 0x15 streams: we set it to 1, because every sync PES begins with a cell (ISO mandates it) and FFmpeg also sets it for data streams. PES_priority 0, copyright 0, original_or_copy 1 follow TRM 0909.4 (R-ST1402TIME-92); FFmpeg writes original_or_copy 0, which is equally legal.

**D-9 MISP offset.** Default: the exact 8.000082 s offset (ST 0603.5 defines MISP = TAI - 8.000082 s; KWIVER uses it too). Option `MispOffset::Legacy8s` for systems that use 8 s; the option name says it is legacy. Constants are integers in microseconds (8_000_082) or nanoseconds (8_000_082_000); no floating point (ST 0603.5 section 5 removed floating point from its own conversion for the same reason).

**D-10 Leap seconds.** Bundle Handbook Table 14 (TAI - UTC = 10 s from 1972-01-01 through 37 s from 2017-01-01) with a validity date taken from the latest IERS Bulletin C at implementation time, plus Equation 2 for 1970-1971; `LeapSeconds::from_list` overrides. Conversions after the validity date succeed but report that the table may be stale. `from_utc` takes a calendar UTC value that may carry second = 60; `from_posix_utc` (a count without leap seconds) documents that the inserted second is ambiguous and maps it to the following second.

**D-11 Rounding.** MISP microseconds from any finer time (UTC with fraction, TAI, GPS, a 90 kHz PTS offset): floor, per ST 0603.4-08. From a Nano Precision Time Stamp: (ns + 500) / 1000, per ST 0603.5 section 7.3. PTS to MISP: misp_us = anchor_us + floor(dticks x 100 / 9), dticks the signed 33-bit difference. MISP to PTS: anchor_pts + round_half_up(dus x 9 / 100), mod 2^33.

**D-12 ST 0604 insertion (VideoTimestamps AddWhenMissing and Rewrite).** One ST 0604 message per access unit, in every access unit (R-ST1402TIME-53, -77). H.264: "MISPmicrosectime"; H.265: the microseconds UUID unless the input already uses the nanoseconds UUID or the caller selects it (one representation per stream, MISP-2018.1-98). The message is a new SEI NAL unit with only this message, 4-byte start code, inserted immediately before the first VCL NAL unit of the access unit (H.264: before a type 14 prefix NAL unit if one directly precedes it), so it follows the AUD, parameter sets and all existing SEI (R-ST1402TIME-82, -86, -87). H.265 nuh_temporal_id_plus1 is copied from the first VCL NAL unit and nuh_layer_id is 0 (R-ST1402TIME-85). Time Status default `9F` (lock unknown): an injector computing times from an anchor cannot assert lock; when the anchor comes from the input's ST 0604 messages, bit 7 is copied from them, and the caller can assert lock. Bits 6 and 5 are set on the first access unit after an anchor discontinuity (forward or backward). Rewrite mode replaces only the 12 status and time bytes of an existing message, keeps its identifier and position, and re-runs emulation prevention on that NAL unit.

**D-13 Video PES after insertion.** The SEI adds 36 (H.264) or 37 (H.265) bytes plus any emulation prevention bytes. If they fit in the adaptation field stuffing of the PES's last TS packet, that stuffing absorbs them and the packet count is unchanged (best for CBR); otherwise one TS packet is added (VBR) or a following null packet is consumed (CBR, R-ST1402TIME-05). TS packets from the insertion point to the end of the PES are regenerated with renumbered video continuity counters, original adaptation field flags and PCRs on their original packets (R-ST1402TIME-11), and PES_packet_length is updated per R-ST1402TIME-14. Only the H.264 NAL HRD (Type II) sees the extra bytes; at 30 fps they add about 9 kbit/s, which the checker reports against the video buffer if HRD parameters are present (standard silent on injection; the original encoder's VCL HRD is unaffected because SEI is non-VCL).

**D-14 Reading ST 0604.** Parse NAL units properly (remove emulation prevention first), accept SEI payloadType 5 with payloadSize >= 28 whose first 16 bytes are any of the three identifiers, in either codec (KWIVER does the same by a raw byte search that also accepts the UUID in H.264). Tolerate separator bytes that are not 0xFF (count a warning) and status bytes with reserved bits not all 1. MPEG-2 user_data is read the same way after `00 00 01 B2`.

**D-15 Anchor from ST 0604.** Each video access unit with both a PTS and an ST 0604 time gives an anchor pair. Re-anchor when the Time Status signals a discontinuity, at a PCR discontinuity, and across a PTS wrap (2^33). An access unit without its own PES PTS (several access units in one PES) gets PTS by adding the measured frame period; sync KLV is never attached to a frame whose PTS was not coded.

**D-16 PSI and PCR in the injector.** PAT and PMT go out every 100 ms by default (R-ST1402TIME-03), rewritten with the new stream and a bumped version_number; PCR packets and PCR_PID are untouched and the KLV PID never carries a PCR (R-ST1402TIME-04). The later elementary-stream Muxer puts the PCR on the video PID (standard silent; usual practice) every 40 ms.

**D-17 Rates and security metadata.** Defaults: sync KLV every frame up to 30 Hz, async 10 Hz, OnChange maximum interval 10 s for static items and never above 30 s (R-ST1402TIME-49, -50). With both carriages active, the ST 0102 set goes only into the sync stream unless configured otherwise (R-ST1402TIME-48).

**D-18 Transport rate.** ST 1402.2 sets no transport rate and no constant-bit-rate requirement; section 7.5 only describes null packets as the CBR tool (R-ST1402TIME-05). VBR output is therefore conformant, and the strict CBR mode exists for links that need it, not for ST 1402.2. The only rate-like constraints are the PSI repetition (R-ST1402TIME-03), PCR interval (R-ST1402TIME-04), the STD delay of sync metadata (R-ST1402TIME-21) and the T-STD buffers (R-ST1402TIME-36).

## 5. Disagreements between sources and with reference implementations

1. FFmpeg (patched) writes no metadata_STD_descriptor for 0x15 KLV streams, against ST 1402-17. We write one (D-1, D-3).
2. FFmpeg sets random_access_indicator = 1 on every fragment cell; ISO's definition (an entry point decodable without previous cells) makes it 1 only on complete and first cells. We follow ISO (3.2).
3. FFmpeg writes original_or_copy = 0, TRM 0909.4 recommends 1. Both legal; we use 1 (D-8).
4. specs/tables/misc.md section 2.3 says an async KLV packet "may start mid-payload"; RP 217 section 4.2 (now available) requires the first item of every PES to be aligned with the PES start. Writers follow RP 217; readers stay tolerant.
5. ST 0604.6 uses the ASCII string "MISPmicrosectime" as uuid_iso_iec_11578 for H.264, while H.264 D.2.7 asks for an ISO/IEC 11578 UUID; MISB's identifier is mandatory for MISP conformance, so we write it (R-ST1402TIME-88).
6. ST 0604.6 no longer requires a timestamp in every frame (ST 0604.3-07 deprecated) but MISP-2023.2 does (MISP-2018.3-116); the profile wins (R-ST1402TIME-77).
7. ST 0603.5 truncates when sampling (ST 0603.4-08) but rounds half up when converting Nano to Precision (section 7.3). Both rules are applied in their own cases (D-11).
8. ST 0603.5 section 6 prints "8.0000822" (the trailing 2 is footnote 2) and Appendix A prints "8.0000 82"; the value is 8.000082 s (confirmed by Handbook Example 1).
9. The ST 0601.19 tag 2 example (`0004 59F4 A6AA 4AA8` = "Oct. 24, 2008. 00:13:29.913") equals the POSIX count of that date: the example shows the MISP count's calendar text, not a UTC instant. MISP time for the UTC instant 2008-10-24T00:13:29.913Z would be 24.999918 s larger (TAI - UTC was 33 s).
10. jMISB converts ST 0603 timestamps to and from `LocalDateTime` as if they were UTC (marked TODO in its source); KWIVER computes MISP from the system clock with a hard-coded 37 s TAI - UTC and the exact 8.000082 s offset, and defaults the Time Status to 0x9F. Our library takes time from the caller and uses a dated table (D-9, D-10, D-12).
11. GStreamer tsmux writes only async KLV (0x06/0xBD + "KLVA" registration) and never a sync stream.
12. FFmpeg's SEI helper (`libavcodec/cbs_sei.c`, `cbs_sei_get_unit`) appends a new message to the first existing SEI NAL unit of the access unit and only creates a new NAL unit (before the first VCL NAL unit) when none exists. In H.265 that can put a user data message into an SEI NAL unit that holds a buffering period or picture timing message, which D.3.1 forbids (R-ST1402TIME-87). We always create a separate NAL unit just before the first VCL NAL unit (D-12). Its `h264_metadata` bitstream filter inserts `sei_user_data` only at seek points, whereas ST 0604 needs every access unit.

## 6. Unresolved

- **U-1** ISO/IEC 13818-4 (conformance, required by MISP-2015.1-50) was not obtained; the checker is built from ISO/IEC 13818-1 T-STD rules only.
- **U-2** ITU-T H.262 is not freely downloadable; the MPEG-2 user_data_start_code (0x000001B2) and the exact syntax position (after the picture header and its extensions) are from general knowledge, not verified against official text. MPEG-2 writing is out of scope for the first release; reading uses the same 28-byte parser.
- **U-3** No MISB document gives values for metadata_input_leak_rate or metadata_buffer_size, and no reference implementation writes the descriptor; D-3 is a reasoned default, not a quoted value. Receivers' reaction to the maximum leak rate code has not been tested with vendor players.
- **U-4** The current leap second state after 2017-01-01 (no leap second has been inserted since, per the Handbook's "Unknown" end date) must be confirmed against the IERS Bulletin C current at implementation time; the table's validity date depends on it.
- **U-5** ST 1402-13 merging relies on reading ISO's sequence_number as one counter per metadata stream (not per PES); the phrase "constituting the metadata_AU_wrapper" could be read per PES. Renumbering per stream satisfies both readings for our own cells; the interpretation matters only for how strictly the checker treats input streams.
- **U-6** The value to write in ST 0601 Item 136 (Leap Seconds, "number of leap seconds since MISP Epoch") is a content question for the ST 0601 topic: by the Handbook conversion, the offset between MISP and UTC since 2017 is 28.999918 s (37 - 8.000082), so 29 rounds it, while ST 0601.19's example shows 30 as an arbitrary sample.
- **U-7** The MISB reference Java code for the time algorithms (Handbook Appendix C, hosted on the MISB PiX site) is not accessible; the formulas rest on Table 17, Equation 2, Table 14 and the Appendix C vector.
- **U-8** SMPTE RP 217 carries an "ARCHIVED OCTOBER 31, 2007" stamp; ST 1402.2 still requires conformance to it (ST 1402-18), so it is treated as normative here.
