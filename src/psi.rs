//! PSI section reassembly and descriptor parsing (ISO/IEC 13818-1 §2.4.4).

/// Largest section_length allowed for PAT, CAT and PMT (2.4.4). Longer
/// values are corruption and would let a hostile table claim 4 KB.
pub(crate) const MAX_PSI_SECTION_LENGTH: usize = 1021;
/// Largest section_length of any section (private sections, DVB SI).
pub(crate) const MAX_SECTION_LENGTH: usize = 4093;

/// Reassembles sections from TS packet payloads on one PID.
///
/// Sections that fit inside a single packet (the common case for PAT/PMT)
/// are handed out straight from the packet without copying.
#[derive(Debug)]
pub(crate) struct SectionAssembler {
    buf: Vec<u8>,
    synced: bool,
    max_len: usize,
}

impl Default for SectionAssembler {
    /// An assembler for PAT, CAT and PMT sections.
    fn default() -> Self {
        Self::with_max_len(MAX_PSI_SECTION_LENGTH)
    }
}

impl SectionAssembler {
    /// An assembler accepting section_length values up to `max_len`.
    pub fn with_max_len(max_len: usize) -> Self {
        SectionAssembler { buf: Vec::new(), synced: false, max_len }
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.synced = false;
    }

    pub fn feed(&mut self, payload: &[u8], pusi: bool, mut on_section: impl FnMut(&[u8])) {
        let mut data = payload;
        if pusi {
            let Some((&ptr, rest)) = payload.split_first() else { return };
            let ptr = usize::from(ptr);
            if ptr > rest.len() {
                self.reset();
                return;
            }
            // Bytes before the pointer finish the section already in progress.
            if self.synced && !self.buf.is_empty() {
                self.buf.extend_from_slice(&rest[..ptr]);
                drain_sections(&self.buf, self.max_len, &mut on_section);
            }
            self.buf.clear();
            self.synced = true;
            data = &rest[ptr..];
        } else if !self.synced {
            return;
        }

        if self.buf.is_empty() {
            match drain_sections(data, self.max_len, &mut on_section) {
                Some(done) => self.buf.extend_from_slice(&data[done..]),
                None => self.synced = false,
            }
        } else {
            self.buf.extend_from_slice(data);
            match drain_sections(&self.buf, self.max_len, &mut on_section) {
                Some(done) => {
                    self.buf.drain(..done);
                }
                None => self.reset(),
            }
        }
        if self.buf.len() > self.max_len + 3 {
            self.reset();
        }
    }
}

/// Emit every complete section in `data`. Returns the offset of the first
/// incomplete section, or `None` if stuffing (0xFF) or an impossible length
/// ended the payload.
fn drain_sections(data: &[u8], max_len: usize, on_section: &mut impl FnMut(&[u8])) -> Option<usize> {
    let mut pos = 0;
    loop {
        let rest = &data[pos..];
        match rest.first() {
            None => return Some(pos),
            Some(0xFF) => return None,
            Some(_) => {}
        }
        if rest.len() < 3 {
            return Some(pos);
        }
        let len = (usize::from(rest[1] & 0x0F) << 8) | usize::from(rest[2]);
        if len > max_len {
            return None;
        }
        let total = 3 + len;
        if rest.len() < total {
            return Some(pos);
        }
        on_section(&rest[..total]);
        pos += total;
    }
}

/// A section using the long (syntax indicator = 1) header.
pub(crate) struct LongSection<'a> {
    pub table_id: u8,
    pub id_ext: u16,
    pub version: u8,
    pub current: bool,
    pub number: u8,
    pub last: u8,
    /// Bytes between the 8-byte header and the CRC.
    pub body: &'a [u8],
}

pub(crate) fn parse_long(s: &[u8]) -> Option<LongSection<'_>> {
    if s.len() < 12 || s[1] & 0x80 == 0 {
        return None;
    }
    Some(LongSection {
        table_id: s[0],
        id_ext: u16::from_be_bytes([s[3], s[4]]),
        version: (s[5] >> 1) & 0x1F,
        current: s[5] & 1 != 0,
        number: s[6],
        last: s[7],
        body: &s[8..s.len() - 4],
    })
}

pub(crate) const D_AC3: u16 = 1 << 0;
pub(crate) const D_EAC3: u16 = 1 << 1;
pub(crate) const D_DTS: u16 = 1 << 2;
pub(crate) const D_AC4: u16 = 1 << 3;
pub(crate) const D_DVB_SUBTITLE: u16 = 1 << 4;
pub(crate) const D_TELETEXT: u16 = 1 << 5;
pub(crate) const D_AAC: u16 = 1 << 6;
pub(crate) const D_TTML: u16 = 1 << 7;

/// The descriptor facts needed for stream identification.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Descriptors {
    /// registration_descriptor (tag 0x05) format_identifier.
    pub registration: Option<[u8; 4]>,
    /// ISO_639_language_descriptor (tag 0x0A), first entry.
    pub language: Option<[u8; 3]>,
    /// metadata_descriptor (tag 0x26) metadata_format_identifier, when
    /// metadata_format is 0xFF.
    pub metadata_format: Option<[u8; 4]>,
    pub flags: u16,
}

impl Descriptors {
    pub fn parse(mut d: &[u8]) -> Self {
        let mut out = Descriptors::default();
        while d.len() >= 2 {
            let tag = d[0];
            let len = usize::from(d[1]);
            let Some(body) = d.get(2..2 + len) else { break };
            match tag {
                0x05 if len >= 4 && out.registration.is_none() => {
                    out.registration = Some([body[0], body[1], body[2], body[3]]);
                }
                0x0A if len >= 3 && out.language.is_none() => {
                    out.language = Some([body[0], body[1], body[2]]);
                }
                0x26 => out.metadata_format = metadata_format(body),
                // DVB (ETSI EN 300 468) descriptors.
                0x46 | 0x56 => out.flags |= D_TELETEXT,
                0x59 => out.flags |= D_DVB_SUBTITLE,
                0x6A => out.flags |= D_AC3,
                0x7A => out.flags |= D_EAC3,
                0x7B => out.flags |= D_DTS,
                0x7C => out.flags |= D_AAC,
                // extension_descriptor: AC-4, DTS-HD, DTS-UHD, TTML.
                0x7F => match body.first() {
                    Some(0x15) => out.flags |= D_AC4,
                    Some(0x0E | 0x21) => out.flags |= D_DTS,
                    Some(0x20) => out.flags |= D_TTML,
                    _ => {}
                },
                _ => {}
            }
            d = &d[2 + len..];
        }
        out
    }

    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }
}

/// Every descriptor in a loop, verbatim.
pub(crate) fn raw_descriptors(mut d: &[u8]) -> Vec<crate::report::Descriptor> {
    let mut out = Vec::new();
    while d.len() >= 2 {
        let len = usize::from(d[1]);
        let Some(body) = d.get(2..2 + len) else { break };
        out.push(crate::report::Descriptor { tag: d[0], data: body.to_vec() });
        d = &d[2 + len..];
    }
    out
}

/// metadata_descriptor: application_format(16) [id(32)] format(8) [id(32)] ...
fn metadata_format(b: &[u8]) -> Option<[u8; 4]> {
    let app = u16::from_be_bytes([*b.first()?, *b.get(1)?]);
    let i = if app == 0xFFFF { 6 } else { 2 };
    if *b.get(i)? != 0xFF {
        return None;
    }
    let id = b.get(i + 1..i + 5)?;
    Some([id[0], id[1], id[2], id[3]])
}
