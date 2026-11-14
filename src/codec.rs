//! Elementary stream identification from stream_type, descriptors and,
//! as a fallback, the first bytes of the PES payload.

use crate::psi::*;
use std::fmt;

/// Broad category of an elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u32)]
pub enum StreamKind {
    /// Video elementary stream.
    Video = 1,
    /// Audio elementary stream.
    Audio = 2,
    /// Timed or untimed metadata (KLV, ID3, SCTE-35, ...).
    Metadata = 3,
    /// Subtitles or teletext.
    Subtitle = 4,
    /// Data that is not audio, video, metadata or subtitles.
    Data = 5,
    /// Nothing identifies the stream.
    Unknown = 0,
}

/// Specific format carried by an elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
#[repr(u32)]
pub enum Codec {
    // Video
    /// MPEG-1 video (ISO/IEC 11172-2).
    Mpeg1Video = 0x101,
    /// MPEG-2 video (H.262).
    Mpeg2Video = 0x102,
    /// MPEG-4 Part 2 visual.
    Mpeg4Visual = 0x103,
    /// H.264 / AVC, including SVC and MVC sub-bitstreams.
    H264 = 0x104,
    /// H.265 / HEVC, including temporal sub-bitstreams.
    H265 = 0x105,
    /// H.266 / VVC.
    H266 = 0x106,
    /// AV1 (registration `AV01`).
    Av1 = 0x107,
    /// SMPTE VC-1.
    Vc1 = 0x108,
    /// JPEG 2000 video.
    Jpeg2000 = 0x109,
    /// JPEG XS video.
    JpegXs = 0x10A,
    /// MPEG-5 EVC video.
    Evc = 0x10B,
    /// MPEG-5 LCEVC enhancement layer.
    Lcevc = 0x10C,
    // Audio
    /// MPEG-1 audio (layers I-III).
    Mpeg1Audio = 0x201,
    /// MPEG-2 audio (ISO/IEC 13818-3).
    Mpeg2Audio = 0x202,
    /// AAC in ADTS framing (stream type 0x0F).
    AacAdts = 0x203,
    /// AAC in LATM/LOAS framing (stream type 0x11).
    AacLatm = 0x204,
    /// MPEG-4 audio without extra framing (stream type 0x1C).
    Mpeg4Audio = 0x205,
    /// AAC signalled by the DVB AAC descriptor.
    Aac = 0x206,
    /// Dolby AC-3.
    Ac3 = 0x207,
    /// Dolby E-AC-3.
    Eac3 = 0x208,
    /// Dolby AC-4.
    Ac4 = 0x209,
    /// DTS family, including DTS-HD.
    Dts = 0x20A,
    /// Dolby TrueHD (Blu-ray).
    TrueHd = 0x20B,
    /// Linear PCM (Blu-ray).
    Lpcm = 0x20C,
    /// Opus (registration `Opus`).
    Opus = 0x20D,
    /// SMPTE ST 302 PCM / AES3 audio (registration `BSSD`).
    Smpte302m = 0x20E,
    /// MPEG-H 3D audio.
    MpegH3dAudio = 0x20F,
    // Metadata
    /// Asynchronous KLV in private PES (SMPTE RP 217 / MISB ST 1402).
    KlvAsync = 0x301,
    /// Synchronous KLV in metadata PES with access units (MISB ST 1402).
    KlvSync = 0x302,
    /// ID3 timed metadata.
    Id3 = 0x303,
    /// SCTE-35 splice information.
    Scte35 = 0x304,
    /// Timeline and External Media Information (Annex U).
    Temi = 0x305,
    /// Metadata stream of an unrecognised format (including 0x16-0x19).
    Metadata = 0x306,
    // Subtitles
    /// DVB subtitles (ETSI EN 300 743).
    DvbSubtitle = 0x401,
    /// EBU teletext.
    Teletext = 0x402,
    /// Blu-ray PGS subtitles.
    PgsSubtitle = 0x403,
    /// DVB TTML subtitles (ETSI EN 303 560).
    TtmlSubtitle = 0x404,
    // Data
    /// DSM-CC data (stream types 0x0A-0x0D, 0x14).
    Dsmcc = 0x501,
    /// SMPTE ST 2038 ancillary data (registration `VANC`).
    Smpte2038 = 0x502,
    /// Private data in PSI sections (stream type 0x05).
    PrivateSections = 0x503,
    /// PES private data with nothing identifying its contents.
    PrivateData = 0x504,
    /// Stream type not recognised and no identifying descriptor.
    Unknown = 0,
}

impl Codec {
    /// Every codec, for iteration.
    pub const ALL: [Codec; 42] = [
        Codec::Mpeg1Video,
        Codec::Mpeg2Video,
        Codec::Mpeg4Visual,
        Codec::H264,
        Codec::H265,
        Codec::H266,
        Codec::Av1,
        Codec::Vc1,
        Codec::Jpeg2000,
        Codec::JpegXs,
        Codec::Evc,
        Codec::Lcevc,
        Codec::Mpeg1Audio,
        Codec::Mpeg2Audio,
        Codec::AacAdts,
        Codec::AacLatm,
        Codec::Mpeg4Audio,
        Codec::Aac,
        Codec::Ac3,
        Codec::Eac3,
        Codec::Ac4,
        Codec::Dts,
        Codec::TrueHd,
        Codec::Lpcm,
        Codec::Opus,
        Codec::Smpte302m,
        Codec::MpegH3dAudio,
        Codec::KlvAsync,
        Codec::KlvSync,
        Codec::Id3,
        Codec::Scte35,
        Codec::Temi,
        Codec::Metadata,
        Codec::DvbSubtitle,
        Codec::Teletext,
        Codec::PgsSubtitle,
        Codec::TtmlSubtitle,
        Codec::Dsmcc,
        Codec::Smpte2038,
        Codec::PrivateSections,
        Codec::PrivateData,
        Codec::Unknown,
    ];

    /// Stable numeric code, also used by the C ABI. Codes are never reused:
    /// video 0x1xx, audio 0x2xx, metadata 0x3xx, subtitles 0x4xx, data 0x5xx,
    /// 0 unknown.
    pub fn code(self) -> u32 {
        self as u32
    }

    /// The codec with this [`code`](Self::code).
    pub fn from_code(code: u32) -> Option<Codec> {
        Self::ALL.iter().copied().find(|c| c.code() == code)
    }

    /// The broad category this format belongs to.
    pub fn kind(self) -> StreamKind {
        use Codec::*;
        match self {
            Mpeg1Video | Mpeg2Video | Mpeg4Visual | H264 | H265 | H266 | Av1 | Vc1 | Jpeg2000
            | JpegXs | Evc | Lcevc => StreamKind::Video,
            Mpeg1Audio | Mpeg2Audio | AacAdts | AacLatm | Mpeg4Audio | Aac | Ac3 | Eac3 | Ac4
            | Dts | TrueHd | Lpcm | Opus | Smpte302m | MpegH3dAudio => StreamKind::Audio,
            KlvAsync | KlvSync | Id3 | Scte35 | Temi | Metadata => StreamKind::Metadata,
            DvbSubtitle | Teletext | PgsSubtitle | TtmlSubtitle => StreamKind::Subtitle,
            Dsmcc | Smpte2038 | PrivateSections | PrivateData => StreamKind::Data,
            Unknown => StreamKind::Unknown,
        }
    }

    /// Human-readable name, e.g. `"H.264/AVC"`.
    pub fn name(self) -> &'static str {
        use Codec::*;
        match self {
            Mpeg1Video => "MPEG-1 Video",
            Mpeg2Video => "MPEG-2 Video",
            Mpeg4Visual => "MPEG-4 Visual",
            H264 => "H.264/AVC",
            H265 => "H.265/HEVC",
            H266 => "H.266/VVC",
            Av1 => "AV1",
            Vc1 => "VC-1",
            Jpeg2000 => "JPEG 2000",
            JpegXs => "JPEG XS",
            Evc => "EVC",
            Lcevc => "LCEVC",
            Mpeg1Audio => "MPEG-1 Audio",
            Mpeg2Audio => "MPEG-2 Audio",
            AacAdts => "AAC (ADTS)",
            AacLatm => "AAC (LATM)",
            Mpeg4Audio => "MPEG-4 Audio",
            Aac => "AAC",
            Ac3 => "AC-3",
            Eac3 => "E-AC-3",
            Ac4 => "AC-4",
            Dts => "DTS",
            TrueHd => "Dolby TrueHD",
            Lpcm => "LPCM",
            Opus => "Opus",
            Smpte302m => "SMPTE 302M PCM",
            MpegH3dAudio => "MPEG-H 3D Audio",
            KlvAsync => "KLV (async)",
            KlvSync => "KLV (sync)",
            Id3 => "ID3",
            Scte35 => "SCTE-35",
            Temi => "TEMI",
            Metadata => "Metadata",
            DvbSubtitle => "DVB Subtitles",
            Teletext => "Teletext",
            PgsSubtitle => "PGS Subtitles",
            TtmlSubtitle => "TTML Subtitles",
            Dsmcc => "DSM-CC",
            Smpte2038 => "SMPTE ST 2038 ANC",
            PrivateSections => "Private sections",
            PrivateData => "Private data",
            Unknown => "Unknown",
        }
    }

    /// Synchronous or asynchronous KLV.
    pub fn is_klv(self) -> bool {
        matches!(self, Codec::KlvSync | Codec::KlvAsync)
    }

    /// True when the declaration was too generic to say what the stream is,
    /// so payload sniffing may refine it.
    pub(crate) fn is_generic(self) -> bool {
        matches!(self, Codec::PrivateData | Codec::Metadata | Codec::Unknown)
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl StreamKind {
    /// Every kind, for iteration.
    pub const ALL: [StreamKind; 6] = [
        StreamKind::Unknown,
        StreamKind::Video,
        StreamKind::Audio,
        StreamKind::Metadata,
        StreamKind::Subtitle,
        StreamKind::Data,
    ];

    /// Stable numeric code, also used by the C ABI.
    pub fn code(self) -> u32 {
        self as u32
    }

    /// The kind with this [`code`](Self::code).
    pub fn from_code(code: u32) -> Option<StreamKind> {
        Self::ALL.iter().copied().find(|k| k.code() == code)
    }

    /// Lower-case name, e.g. `"video"`.
    pub fn name(self) -> &'static str {
        match self {
            StreamKind::Video => "video",
            StreamKind::Audio => "audio",
            StreamKind::Metadata => "metadata",
            StreamKind::Subtitle => "subtitle",
            StreamKind::Data => "data",
            StreamKind::Unknown => "unknown",
        }
    }
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What the identification of a stream was based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u32)]
pub enum Basis {
    /// The PMT stream_type alone.
    StreamType = 1,
    /// PMT descriptors (registration, metadata, DVB, ...).
    Descriptor = 2,
    /// Inspection of the PES payload; the PMT did not say.
    Payload = 3,
}

impl Basis {
    /// Stable numeric code, also used by the C ABI.
    pub fn code(self) -> u32 {
        self as u32
    }
}

const KLVA: [u8; 4] = *b"KLVA";
const HDMV: [u8; 4] = *b"HDMV";
const ID3: [u8; 4] = *b"ID3 ";
/// SMPTE universal label prefix that starts every KLV key.
const KLV_UL_PREFIX: [u8; 4] = [0x06, 0x0E, 0x2B, 0x34];

pub(crate) fn identify(stream_type: u8, es: &Descriptors, prog: &Descriptors) -> (Codec, Basis) {
    use Codec::*;
    if prog.registration == Some(HDMV) {
        if let Some(c) = bluray(stream_type) {
            return (c, Basis::Descriptor);
        }
    }
    let codec = match stream_type {
        0x01 => Mpeg1Video,
        0x02 => Mpeg2Video,
        0x03 => Mpeg1Audio,
        0x04 => Mpeg2Audio,
        0x05 => PrivateSections,
        0x0A..=0x0D | 0x14 => Dsmcc,
        0x0F => AacAdts,
        0x10 => Mpeg4Visual,
        0x11 => AacLatm,
        0x15 => return metadata_pes(es),
        0x16..=0x19 => Metadata,
        // AVC plus its SVC, MVC and MVCD sub-bitstreams and stereo views.
        0x1B | 0x1F | 0x20 | 0x23 | 0x26 => H264,
        0x1C => Mpeg4Audio,
        0x21 => Jpeg2000,
        0x22 => Mpeg2Video, // stereoscopic additional view
        0x27 => Temi,
        // HEVC plus temporal, layered (SHVC/MV-HEVC) and tile sub-streams.
        0x24 | 0x25 | 0x28..=0x2B | 0x31 => H265,
        0x2D | 0x2E => MpegH3dAudio,
        0x32 => JpegXs,
        0x33 | 0x34 => H266,
        0x35 => Evc,
        0x36 => Lcevc,
        0x81 => Ac3,
        0x86 => Scte35,
        0x87 => Eac3,
        0xEA => Vc1,
        0x06 => return private(stream_type, es, prog).unwrap_or((PrivateData, Basis::StreamType)),
        _ => return private(stream_type, es, prog).unwrap_or((Unknown, Basis::StreamType)),
    };
    (codec, Basis::StreamType)
}

/// Blu-ray (BDAV/M2TS) assignments in the user-private range.
fn bluray(stream_type: u8) -> Option<Codec> {
    use Codec::*;
    Some(match stream_type {
        0x80 => Lpcm,
        0x81 => Ac3,
        0x82 | 0x85 | 0x86 | 0xA2 => Dts,
        0x83 => TrueHd,
        0x84 | 0xA1 => Eac3,
        0x90 => PgsSubtitle,
        0xEA => Vc1,
        _ => return None,
    })
}

/// stream_type 0x15: metadata carried in PES (synchronous per MISB ST 1402).
fn metadata_pes(es: &Descriptors) -> (Codec, Basis) {
    match es.metadata_format.or(es.registration) {
        Some(KLVA) => (Codec::KlvSync, Basis::Descriptor),
        Some(ID3) => (Codec::Id3, Basis::Descriptor),
        _ => (Codec::Metadata, Basis::StreamType),
    }
}

/// Private and user-defined stream types, identified by descriptors.
fn private(stream_type: u8, es: &Descriptors, prog: &Descriptors) -> Option<(Codec, Basis)> {
    use Codec::*;
    let codec = if es.has(D_AC3) {
        Ac3
    } else if es.has(D_EAC3) {
        Eac3
    } else if es.has(D_AC4) {
        Ac4
    } else if es.has(D_DTS) {
        Dts
    } else if es.has(D_AAC) {
        Aac
    } else if es.has(D_DVB_SUBTITLE) {
        DvbSubtitle
    } else if es.has(D_TTML) {
        TtmlSubtitle
    } else if es.has(D_TELETEXT) {
        Teletext
    } else {
        match es.registration.or(es.metadata_format).as_ref().map(|r| &r[..]) {
            Some(b"KLVA") => KlvAsync,
            Some(b"AC-3") => Ac3,
            Some(b"EAC3") => Eac3,
            Some(b"DTS1" | b"DTS2" | b"DTS3") => Dts,
            Some(b"Opus") => Opus,
            Some(b"BSSD") => Smpte302m,
            Some(b"CUEI") => Scte35,
            Some(b"ID3 ") => Id3,
            Some(b"HEVC") => H265,
            Some(b"AV01") => Av1,
            Some(b"VC-1") => Vc1,
            Some(b"VANC") => Smpte2038,
            // RP 217 also allows the KLVA registration at program level, for
            // private PES streams that carry no registration of their own.
            None if stream_type == 0x06 && prog.registration == Some(KLVA) => KlvAsync,
            _ => return None,
        }
    };
    Some((codec, Basis::Descriptor))
}

/// Guess a generically declared stream's format from its first PES.
/// `es` is the payload after the PES header.
pub(crate) fn sniff(pes_stream_id: u8, es: &[u8]) -> Option<Codec> {
    if pes_stream_id == 0xFC {
        // metadata_stream: payload is metadata access-unit cells with a
        // 5-byte cell header before the KLV.
        if es.get(5..9) == Some(&KLV_UL_PREFIX) || es.starts_with(&KLV_UL_PREFIX) {
            return Some(Codec::KlvSync);
        }
        return None;
    }
    if es.starts_with(&KLV_UL_PREFIX) {
        return Some(Codec::KlvAsync);
    }
    if es.starts_with(b"ID3") {
        return Some(Codec::Id3);
    }
    None
}
