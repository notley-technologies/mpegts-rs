# Standards notes for the write path

These documents record what the MPEG-TS write path (the `mux` feature: TS, PSI and PES writers, metadata access unit cells, later KLV injection) must do to conform, settled before any code was written. The code cites the rule ids defined here. Every rule cites the standard, section or requirement id, with a short verbatim quote.

| File | Covers |
|---|---|
| [DECISIONS.md](DECISIONS.md) | The consolidated decisions and quick-reference values. Read this first; it wins over the topic files where they differ. |
| [iso13818.md](iso13818.md) | ISO/IEC 13818-1 (ITU-T H.222.0 10/2014): TS packets, continuity, adaptation field, PCR, PSI, PES, metadata descriptors and cells, T-STD. |
| [st1402-time.md](st1402-time.md) | MISB ST 1402.2 sync and async KLV carriage, SMPTE RP 217, MISB ST 0603.5 MISP time and MISB ST 0604.6 timestamps in video. |

The KLV content rules (ST 0601, ST 0102, ST 0903 encoding) live with the `misb` crate: [misb-rs/docs/standards](https://github.com/notley-technologies/misb-rs/tree/main/docs/standards). Paths such as `specs/src/H.222.0-201410.txt:123` point into a local copy of the official documents, which are not redistributed; the document, section and quote identify each source without it.
