# Phase 0 decisions (consolidated)

Phase 0 of the muxing plan is closed. Four topic documents hold the rules, each rule with a verbatim quote and a source location; every quote was checked mechanically (`python3 -I specs/phase0/check_quotes.py`: 790 quotes, all found at the cited line).

| Topic | File | Rules | Implements |
|---|---|---|---|
| ISO/IEC 13818-1 (H.222.0 10/2014): TS, PCR, PSI, PES, metadata descriptors and cells, T-STD | [iso13818.md](iso13818.md) | 174 | `mpegts::mux`, checker |
| MISB ST 1402.2 carriage, SMPTE RP 217, MISB ST 0603.5 time, MISB ST 0604.6 video timestamps | [st1402-time.md](st1402-time.md) | 92 | `mpegts::mux`, `misb` (time, ST 0604 payload), `misb::mux` |
| MISB ST 0601.19 encoding, ST 0107.4, ST 0902.8 minimum set | [st0601-encode.md](https://github.com/notley-technologies/misb-rs/blob/main/docs/standards/st0601-encode.md) | 86 | `misb::encode` |
| MISB ST 0102.12, ST 0903.6 (VMTI), ST 1201.5 IMAPB, ST 1204.3 encoding | [nested-encode.md](https://github.com/notley-technologies/misb-rs/blob/main/docs/standards/nested-encode.md) | 141 | `misb::encode` |

This file records (1) where two topics disagreed and what wins, (2) values settled across topics, (3) corrections to the plan, and (4) prerequisites before phase 1. Where this file and a topic document disagree, this file wins.

## 1. Cross-topic conflicts resolved

**X-1 Registration descriptor on sync KLV streams.** `iso13818.md` D-ISO13818-01 proposed writing a "KLVA" registration_descriptor on 0x15 streams by default; `st1402-time.md` D-1 proposed not to. **Decision: not written by default; option `sync_registration_descriptor`.** ST 1402.2 identifies sync KLV through metadata_format_identifier (ST 1402.1-26) and uses the registration descriptor only as RP 217 async signalling. FFmpeg's demuxer (`METADATA_types`, `specs/ref/ff_mpegts.c:948`), GStreamer's and our own `mpegts` (`mpegts/src/codec.rs`, stream_type 0x15 branch) all identify sync KLV from the metadata_descriptor alone, so nothing is gained by default.

**X-2 metadata_STD_descriptor values.** `iso13818.md` D-ISO13818-04 proposed 100 Mbit/s and 128 KiB; `st1402-time.md` D-3 proposed the maximum leak rate and 1 MiB. **Decision: `27 09 FF FF FF C0 04 00 C0 00 00`**: metadata_input_leak_rate 0x3FFFFF (1,677,721,200 bit/s), metadata_buffer_size 0x000400 (1 MiB), metadata_output_leak_rate 0. Reason: TB_n is 512 bytes and a pass-through injector cannot know the instantaneous transport rate of a VBR input, so only the maximum leak rate rules out TB_n overflow; B_n must hold up to 1 s of metadata (ST 1402-12), and 1 MiB covers about 1 MB/s (for example 30 Hz VMTI packets of 30 KB). Both values are configurable; the writer tracks B_n occupancy against the declared size and applies the overflow policy instead of exceeding it, and spaces TS packets if a lower leak rate is configured. No reference implementation writes this descriptor, so there is no prior art to match (FFmpeg omits it, violating ST 1402-17).

**X-3 PAT/PMT repetition in the injector.** `iso13818.md` D-ISO13818-12/-20 kept the input's cadence and the PAT untouched (ISO sets no rate); `st1402-time.md` D-16 required 100 ms (ST 1402.2 requires more than 4 per second and recommends 8, R-ST1402TIME-03). **Decision: keep every input PAT/PMT occurrence (the target PMT rewritten), and additionally insert copies so that neither the PAT nor the target PMT gap exceeds `psi_max_interval` (default 100 ms).** The injector therefore owns the continuity counters of PID 0 and the target PMT PID (PAT content is never changed). Pass-through invariance excludes those PIDs. Extra PSI packets are spaced per the TB_sys model (D-ISO13818-21).

**X-4 Async PES packing.** `specs/tables/misc.md` said an async KLV packet may start mid-payload. SMPTE RP 217 (now obtained, `specs/src/SMPTE-RP217-2001.txt`) requires each PES to start with a whole KLV item and not to split an item that fits. **Decision: one KLV packet per async PES by default; only packets larger than 65,532 data bytes (PES_packet_length is never 0 for private data, and the 3 bytes of flags and header length follow it) are split, with data_alignment_indicator 1 on the first PES and 0 on continuations** (ST 1402-20/-21). Readers stay tolerant of mid-payload starts.

**X-5 Item 136 (Leap Seconds) value and sign.** Left open by both `st0601-encode.md` U-08 and `st1402-time.md` U-6. ST 0601.19 8.136: "Number of leap seconds since MISP Epoch to use when converting Precision Time Stamp (Item 2) to UTC" (`specs/src/ST0601.19.txt:6966`); the Handbook defines MISP = TAI - 8.000082 s (Table 17, `specs/src/MISP-2025.1-Motion-Imagery-Handbook.txt:2600`). **Decision: the value is round(TAI-UTC - 8.000082 s) from the leap second table, which is 29 for instants since 2017-01-01; UTC = tag 2 - (item 136 x 1,000,000) us (approximate to 82 us; exact conversion uses the table).** The ST 0601 example value 30 is a sample. Item 136 is optional and not in the ST 0902 minimum set: not written by default; option `leap_seconds_item`. The misb decoder's `Tag::LeapSeconds` doc has the sign backwards (it says "plus") and must be fixed (prerequisite P-3).

**X-6 data_alignment_indicator of sync KLV PES packets (decided 2026-11-20).** `st1402-time.md` D-8 wrote 1 on every sync PES; `iso13818.md` D-ISO13818-08 wrote 1 only when the PES begins an access unit. Both standards are silent for stream_id 0xFC. **Decision: D-ISO13818-08. 1 when the PES packet's first cell has cell_fragment_indication '11' or '10' (it begins an access unit), 0 on the PES packets of middle and last fragments.** Reason: it mirrors the normative asynchronous rule (ST 1402-20/-21: 1 exactly when the PES contains the beginning of a KLV item) and the cell-level random_access_indicator (R-ST1402TIME-32), so a receiver can resynchronise on it; "always 1" carries no information. It only differs from D-8 for access units above 65,522 bytes. Implemented in `mpegts::mux::PesWriter`.

## 2. Values settled (quick reference)

| Subject | Value | Source |
|---|---|---|
| ST 0601 key written | `06 0E 2B 34 02 0B 01 01 0E 01 03 01 01 00 00 00` (byte 8 = 0x01) | st0601-encode |
| Mandatory ST 0601 items | 2 (first), 65 = `41 01 13`, 1 (last); all others optional | st0601-encode |
| Item order | ascending tag between 2 and 1 (reproduces ST 0902.8 Annex C packets byte for byte); 102 right after the items it describes | st0601-encode |
| Lengths and tags | minimal BER lengths and minimal BER-OID tags everywhere | st0601-encode, nested-encode |
| Linear mapped values | round half away from zero (reproduces all 64 published examples) | st0601-encode |
| IMAPB values | truncate (ST 1201.5); special values `C8` +inf, `E8` -inf, `D0` NaN, `E0` below min, `E1` above max, `C1` VMTI invalid | nested-encode |
| ST 0102 nested (tag 48) | items 1, 2, 3, 12, 13, 22 required; version `16 02 00 0C`; 1-byte tags; in every packet by default | nested-encode |
| ST 0903 nested (tag 74) | version `04 01 06`; item 6 always; no checksum; target ids BER-OID up to 9 bytes, unique | nested-encode |
| Packet size cap (encoder default) | 65,535 bytes | st0601-encode |
| Sync KLV stream | stream_type 0x15, stream_id 0xFC, PTS only (14-byte PES header), one AU = one KLV packet per cell per PES | iso13818, st1402-time |
| Sync PMT ES descriptors | `26 09 01 00 FF 4B 4C 56 41 00 0F` (per service) + `27 09 FF FF FF C0 04 00 C0 00 00` (per stream) | X-1, X-2 |
| AU cell flags | `DF` complete, `9F` first, `0F` middle, `4F` last; sequence_number one counter per PID mod 256; PES data_alignment_indicator 1 only on the PES that begins the AU (X-6) | iso13818, st1402-time |
| Max cell data | 65,522 bytes; every PES of a fragmented AU carries the AU's PTS | iso13818 |
| Async KLV stream | stream_type 0x06, stream_id 0xBD, no PTS, registration `05 04 4B 4C 56 41` | iso13818, st1402-time |
| Sync insertion window | arrival in [PTS - 1 s + 10 ms, PTS - 1 ms]; before the frame's first packet when that fits, else the first later packet that fits | iso13818 D-14, st1402-time D-7 |
| Existing sync KLV in the input | merge as a new service on the existing 0x15 PID (ST 1402-13); renumber that PID's CC and cell sequence numbers; never change input AU bytes | st1402-time D-5 |
| PSI repetition | at most 100 ms between PAT copies and between target PMT copies | X-3 |
| PCR | input PCRs never touched; KLV PID never carries PCR; muxer (later) puts PCR on the video PID every 40 ms | iso13818, st1402-time |
| MISP time | MISP = TAI - 8.000082 s exactly, integer microsecond constants; MISP = UTC + 28.999918 s since 2017 | st1402-time |
| ST 0604 SEI | H.264 user_data_unregistered "MISPmicrosectime", H.265 UUID `a8687dd4-d759-3758-a5ce-f0338b6545f1` (us) / `cf848278-ee23-306c-9265-e8fef22fb8b8` (ns); status byte, 8-byte time with `FF` after every 2 bytes; own NAL before the first slice; every access unit; status default `9F` | st1402-time D-12 |
| Rates | no standard rate; defaults: sync every frame up to 30 Hz, async 10 Hz, every known item at least every 30 s, static items every 10 s | st0601-encode, st1402-time D-17 |
| CBR | not required by any standard in scope; strict CBR is a link option | st1402-time D-18 |

## 3. Corrections to the muxing plan (applied there)

1. Sync KLV is inserted before its video frame only when that respects the 1 s delay limit (ST 1402-12, ISO 2.12.10); AVC/HEVC video may lead by up to 10 s.
2. `ExistingKlv::Keep` means merging into the existing 0x15 stream as a new service (ST 1402-13), not adding a second 0x15 PID.
3. IMAPB round-trip tolerance: decoded value in [v - 1 LSB, v] (truncation), not within half an LSB. Linear mapped values do round-trip within half an LSB.
4. Recorded packets (`.klv` sources): tag 2 is never modified; PTS = anchor^-1(tag 2) rounded to the nearest 90 kHz tick, not snapped to a frame (option to snap).
5. PSI cadence per X-3; registration descriptor per X-1; STD descriptor per X-2.
6. Validators available locally: patched FFmpeg at `ffmpeg-klv-fix/build/ffprobe` (series applied). TSDuck is not installed; install it in CI before phase 3.

## 4. Prerequisites before phase 1 (decoder fixes the encoder depends on)

These come from the misb review of 2026-11-14 and were found again independently in phase 0. They block exact decode -> encode round trips:

- **P-1** A string value of exactly `00` must decode to an empty string, not "\0" (ST 0107.4 6.3.3.2).
- **P-2** The decoded value model must keep IMAPB below-minimum vs above-maximum (`E0`/`E1`), the VMTI "Invalid" signal (`C1`), and "Reserved" vs "Out of Range" codes distinct, or re-encoding must copy those items from their original bytes.
- **P-3** Fix the sign in the `Tag::LeapSeconds` doc (X-5) and the MISP-to-UTC offset stated in `include/misb.h` (it is off by 10 s).
- **P-4** Tag 47 must be exactly 1 byte with bits 6-7 zero (ST 0601.13-29), so encode and decode agree.

## 5. Still open (non-blocking)

- ST 0107.5 and ISO/IEC 13818-4 could not be obtained; ST 0107.4 is cited instead. The MPEG-2 video user_data start code for ST 0604 is from general knowledge.
- ST 0902.8 lists tag 104 as 2 bytes where ST 0601.19 needs 3 or more; the default profile uses tag 75 instead.
- ST 0903.6 section 9.2 "byte 6 = 0x2B" contradicts the VMTI key's `0x0B`; the key wins.
- The leap second table must be checked against the current IERS Bulletin C when implemented.
- Text extraction of some H.222.0 tables was not checked against the rendered PDF (pages 31-33, 109-113, 151-152); the field widths agree with ST 1402.2 and both reference muxers.
