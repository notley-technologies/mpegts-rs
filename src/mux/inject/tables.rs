//! What the input's PSI says: the PAT, every PMT it points to, and the CAT
//! (ISO/IEC 13818-1 2.4.4), tracked from the packets the injector sees.
//!
//! The injector parses PSI itself instead of running a [`Demuxer`](crate::Demuxer):
//! it needs the exact section bytes and the packet that completes each
//! section, and nothing else the demuxer does per packet.

use super::super::spec::metadata_service_id;
use crate::codec::{identify, Codec};
use crate::crc32_mpeg2;
use crate::packet::NULL_PID;
use crate::psi::{parse_long, Descriptors, SectionAssembler};
use crate::{StreamKind, PACKET_SIZE};

pub(super) const PAT_PID: u16 = 0x0000;
pub(super) const CAT_PID: u16 = 0x0001;

/// A set of PIDs (8192 bits).
#[derive(Debug, Clone)]
pub(super) struct Pids(Box<[u64; 128]>);

impl Default for Pids {
    fn default() -> Self {
        Pids(Box::new([0; 128]))
    }
}

impl Pids {
    pub fn set(&mut self, pid: u16) {
        let pid = usize::from(pid & 0x1FFF);
        self.0[pid >> 6] |= 1 << (pid & 63);
    }

    pub fn get(&self, pid: u16) -> bool {
        let pid = usize::from(pid & 0x1FFF);
        self.0[pid >> 6] & (1 << (pid & 63)) != 0
    }

    pub fn clear(&mut self) {
        self.0.fill(0);
    }
}

/// One elementary stream entry of a PMT, as offsets into the section.
#[derive(Debug, Clone)]
pub(super) struct Es {
    pub pid: u16,
    /// The ES_info descriptor loop.
    pub desc: (usize, usize),
    pub codec: Codec,
}

/// A program's current PMT section, parsed.
#[derive(Debug, Clone)]
pub(super) struct Pmt {
    pub raw: Vec<u8>,
    pub version: u8,
    /// PCR_PID; 0x1FFF for none.
    pub pcr_pid: u16,
    /// program_info descriptors.
    pub info: (usize, usize),
    pub streams: Vec<Es>,
}

impl Pmt {
    pub fn info(&self) -> &[u8] {
        &self.raw[self.info.0..self.info.1]
    }

    pub fn desc(&self, es: &Es) -> &[u8] {
        &self.raw[es.desc.0..es.desc.1]
    }

    pub fn pcr(&self) -> Option<u16> {
        (self.pcr_pid != NULL_PID).then_some(self.pcr_pid)
    }

    /// The first video stream in PMT order.
    pub fn video(&self) -> Option<&Es> {
        self.streams
            .iter()
            .find(|s| s.codec.kind() == StreamKind::Video)
    }

    /// Parse a PMT section whose CRC and header were checked. Fails when
    /// the program_info or an ES_info loop runs past the section.
    fn parse(raw: &[u8]) -> Option<Pmt> {
        let end = raw.len().checked_sub(4)?;
        if end < 12 {
            return None;
        }
        let pcr_pid = u16::from_be_bytes([raw[8] & 0x1F, raw[9]]);
        let info_len = (usize::from(raw[10] & 0x0F) << 8) | usize::from(raw[11]);
        let info = (12, 12 + info_len);
        if info.1 > end {
            return None;
        }
        let prog = Descriptors::parse(&raw[info.0..info.1]);
        let mut streams = Vec::new();
        let mut at = info.1;
        // A few trailing bytes too short for an entry are ignored, as
        // demuxers do; an entry that overruns makes the section unusable.
        while end - at >= 5 {
            let stream_type = raw[at];
            let pid = u16::from_be_bytes([raw[at + 1] & 0x1F, raw[at + 2]]);
            let es_len = (usize::from(raw[at + 3] & 0x0F) << 8) | usize::from(raw[at + 4]);
            let desc = (at + 5, at + 5 + es_len);
            if desc.1 > end {
                return None;
            }
            let (codec, _) = identify(
                stream_type,
                &Descriptors::parse(&raw[desc.0..desc.1]),
                &prog,
            );
            streams.push(Es { pid, desc, codec });
            at = desc.1;
        }
        Some(Pmt {
            raw: raw.to_vec(),
            version: (raw[5] >> 1) & 0x1F,
            pcr_pid,
            info,
            streams,
        })
    }
}

/// A program listed in the PAT.
#[derive(Debug, Clone)]
pub(super) struct Program {
    pub number: u16,
    pub pmt_pid: u16,
    pub pmt: Option<Pmt>,
}

/// The current PAT: every section of its version.
#[derive(Debug, Clone, Default)]
pub(super) struct Pat {
    pub version: u8,
    pub sections: Vec<Vec<u8>>,
    pub network_pid: Option<u16>,
}

/// Section reassembly on one PSI PID, with the continuity checks that keep
/// a lost or duplicated packet from corrupting a section (ISO/IEC 13818-1
/// 2.4.3.3).
#[derive(Debug)]
struct PsiPid {
    pid: u16,
    asm: SectionAssembler,
    last_cc: Option<u8>,
    last: [u8; PACKET_SIZE],
}

impl PsiPid {
    fn new(pid: u16) -> Self {
        PsiPid {
            pid,
            asm: SectionAssembler::default(),
            last_cc: None,
            last: [0; PACKET_SIZE],
        }
    }
}

/// Complete sections collected from one packet. The buffers are reused.
#[derive(Debug, Default)]
pub(super) struct Sections {
    bufs: Vec<Vec<u8>>,
    n: usize,
}

impl Sections {
    pub fn clear(&mut self) {
        self.n = 0;
    }

    fn push(&mut self, s: &[u8]) {
        if self.n == self.bufs.len() {
            self.bufs.push(Vec::new());
        }
        let b = &mut self.bufs[self.n];
        b.clear();
        b.extend_from_slice(s);
        self.n += 1;
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn get(&self, i: usize) -> &[u8] {
        &self.bufs[i]
    }
}

/// What applying a section changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Applied {
    /// Nothing the injector tracks changed (an identical repeat, a section
    /// of another table, a "next" section).
    Same,
    /// The PAT, a PMT or the CAT changed.
    Changed,
    /// A PAT, PMT or CAT section with a bad CRC.
    CrcError,
}

/// The input's PSI.
#[derive(Debug, Default)]
pub(super) struct Tables {
    pub pat: Option<Pat>,
    /// Sections of the PAT version being collected.
    pat_set: Vec<Vec<u8>>,
    pat_set_version: Option<u8>,
    /// Programs in PAT order, program_number 0 excluded.
    pub programs: Vec<Program>,
    /// CA PIDs named by the CAT.
    pub cat_pids: Vec<u16>,
    pmt_pids: Pids,
    psi: Vec<PsiPid>,
}

impl Tables {
    pub fn new() -> Self {
        Tables {
            psi: vec![PsiPid::new(PAT_PID), PsiPid::new(CAT_PID)],
            ..Tables::default()
        }
    }

    /// PIDs whose sections are tracked: the PAT, the CAT and every PMT PID.
    pub fn is_psi(&self, pid: u16) -> bool {
        pid == PAT_PID || pid == CAT_PID || self.pmt_pids.get(pid)
    }

    pub fn program(&self, number: u16) -> Option<&Program> {
        self.programs.iter().find(|p| p.number == number)
    }

    /// Forget partial sections (the input is fed again from another point).
    pub fn reset_assemblers(&mut self) {
        for p in &mut self.psi {
            p.asm.reset();
            p.last_cc = None;
        }
    }

    /// Feed one packet of a PSI PID; complete sections go to `out`.
    pub fn assemble(&mut self, p: &[u8; PACKET_SIZE], out: &mut Sections) {
        let pid = u16::from_be_bytes([p[1] & 0x1F, p[2]]);
        let Some(st) = self.psi.iter_mut().find(|s| s.pid == pid) else {
            return;
        };
        // A packet with transport_error_indicator may be damaged anywhere
        // (2.4.3.3): drop what is being assembled.
        if p[1] & 0x80 != 0 {
            st.asm.reset();
            st.last_cc = None;
            return;
        }
        let afc = (p[3] >> 4) & 3;
        if afc & 1 == 0 || p[3] & 0xC0 != 0 {
            // No payload, or scrambled (PSI never is, R-ISO13818-071).
            return;
        }
        let mut start = 4;
        let mut discontinuity = false;
        if afc & 2 != 0 {
            start = 5 + usize::from(p[4]);
            if start >= PACKET_SIZE {
                return;
            }
            discontinuity = p[4] > 0 && p[5] & 0x80 != 0;
        }
        let cc = p[3] & 0x0F;
        if let (Some(last), false) = (st.last_cc, discontinuity) {
            if cc == last && st.last[..] == p[..] {
                return; // A duplicate (R-ISO13818-011).
            }
            if cc != (last + 1) & 0x0F {
                st.asm.reset(); // Lost packets: the section in progress is gone.
            }
        }
        st.last_cc = Some(cc);
        st.last.copy_from_slice(p);
        let pusi = p[1] & 0x40 != 0;
        st.asm.feed(&p[start..], pusi, |s| out.push(s));
    }

    /// Apply a complete section that arrived on `pid`.
    pub fn apply(&mut self, pid: u16, s: &[u8]) -> Applied {
        let Some(sec) = parse_long(s) else {
            return Applied::Same;
        };
        match (pid, sec.table_id) {
            (PAT_PID, 0x00) => {
                if crc32_mpeg2(s) != 0 {
                    return Applied::CrcError;
                }
                if !sec.current {
                    return Applied::Same;
                }
                self.apply_pat(sec.version, sec.number, sec.last, s)
            }
            (CAT_PID, 0x01) => {
                if crc32_mpeg2(s) != 0 {
                    return Applied::CrcError;
                }
                if !sec.current {
                    return Applied::Same;
                }
                let mut pids = [0u16; 64];
                let mut n = 0;
                each_descriptor(sec.body, |tag, d| {
                    if tag == 0x09 && d.len() >= 4 && n < pids.len() {
                        pids[n] = u16::from_be_bytes([d[2] & 0x1F, d[3]]);
                        n += 1;
                    }
                });
                if pids[..n] == self.cat_pids[..] {
                    Applied::Same
                } else {
                    self.cat_pids = pids[..n].to_vec();
                    Applied::Changed
                }
            }
            (_, 0x02) if self.pmt_pids.get(pid) => {
                let number = sec.id_ext;
                let Some(prog) = self
                    .programs
                    .iter_mut()
                    .find(|p| p.number == number && p.pmt_pid == pid)
                else {
                    return Applied::Same;
                };
                if crc32_mpeg2(s) != 0 {
                    return Applied::CrcError;
                }
                // A program definition is one section (R-ISO13818-087,
                // R-ISO13818-095); "next" sections do not apply yet
                // (R-ISO13818-094).
                if !sec.current || sec.number != 0 || sec.last != 0 {
                    return Applied::Same;
                }
                if prog.pmt.as_ref().is_some_and(|m| m.raw == s) {
                    return Applied::Same;
                }
                match Pmt::parse(s) {
                    Some(pmt) => {
                        prog.pmt = Some(pmt);
                        Applied::Changed
                    }
                    None => Applied::Same,
                }
            }
            _ => Applied::Same,
        }
    }

    fn apply_pat(&mut self, version: u8, number: u8, last: u8, s: &[u8]) -> Applied {
        let count = usize::from(last) + 1;
        if self.pat_set_version != Some(version) || self.pat_set.len() != count {
            self.pat_set = vec![Vec::new(); count];
            self.pat_set_version = Some(version);
        }
        let n = usize::from(number);
        if n >= count {
            return Applied::Same;
        }
        if self.pat_set[n] != s {
            self.pat_set[n] = s.to_vec();
        }
        if self.pat_set.iter().any(|v| v.is_empty()) {
            return Applied::Same;
        }
        if self
            .pat
            .as_ref()
            .is_some_and(|p| p.version == version && p.sections == self.pat_set)
        {
            return Applied::Same;
        }
        let mut network_pid = None;
        let mut entries: Vec<(u16, u16)> = Vec::new();
        for raw in &self.pat_set {
            let Some(sec) = parse_long(raw) else { continue };
            for e in sec.body.chunks_exact(4) {
                let number = u16::from_be_bytes([e[0], e[1]]);
                let pid = u16::from_be_bytes([e[2] & 0x1F, e[3]]);
                if !(0x0010..NULL_PID).contains(&pid) {
                    // program_map_PID and network_PID are 0x0010 to 0x1FFE
                    // (R-ISO13818-084); a program elsewhere cannot be
                    // rewritten and is ignored.
                    continue;
                }
                if number == 0 {
                    network_pid = Some(pid);
                } else if !entries.iter().any(|&(n, _)| n == number) {
                    // A program_number appears once (R-ISO13818-083): the
                    // first entry wins.
                    entries.push((number, pid));
                }
            }
        }
        let old = std::mem::take(&mut self.programs);
        self.programs = entries
            .iter()
            .map(|&(number, pmt_pid)| {
                match old
                    .iter()
                    .find(|p| p.number == number && p.pmt_pid == pmt_pid)
                {
                    Some(p) => p.clone(),
                    None => Program {
                        number,
                        pmt_pid,
                        pmt: None,
                    },
                }
            })
            .collect();
        self.pmt_pids.clear();
        for p in &self.programs {
            self.pmt_pids.set(p.pmt_pid);
        }
        self.psi.retain(|s| {
            s.pid == PAT_PID || s.pid == CAT_PID || entries.iter().any(|&(_, pid)| pid == s.pid)
        });
        for &(_, pid) in &entries {
            if !self.psi.iter().any(|s| s.pid == pid) {
                self.psi.push(PsiPid::new(pid));
            }
        }
        self.pat = Some(Pat {
            version,
            sections: self.pat_set.clone(),
            network_pid,
        });
        Applied::Changed
    }

    /// Every PMT that has arrived.
    pub fn pmts(&self) -> impl Iterator<Item = (&Program, &Pmt)> {
        self.programs
            .iter()
            .filter_map(|p| p.pmt.as_ref().map(|m| (p, m)))
    }

    /// Every PAT program's PMT has arrived.
    pub fn complete(&self) -> bool {
        self.pat.is_some() && self.programs.iter().all(|p| p.pmt.is_some())
    }

    /// PIDs the input uses according to its tables (ISO/IEC 13818-1 Table
    /// 2-3 and D-ISO13818-11): PMT PIDs, the network PID, every elementary
    /// and PCR PID, and the CA PIDs of the CAT and of CA descriptors.
    pub fn used_pids(&self, out: &mut Pids) {
        if let Some(pid) = self.pat.as_ref().and_then(|p| p.network_pid) {
            out.set(pid);
        }
        for &pid in &self.cat_pids {
            out.set(pid);
        }
        for p in &self.programs {
            out.set(p.pmt_pid);
        }
        for (_, m) in self.pmts() {
            out.set(m.pcr_pid);
            ca_pids(m.info(), out);
            for s in &m.streams {
                out.set(s.pid);
                ca_pids(m.desc(s), out);
            }
        }
    }

    /// metadata_service_ids the input uses (D-ISO13818-03): those of every
    /// metadata_descriptor and metadata_pointer_descriptor in any PMT,
    /// except in the ES loops of `skip` streams of program `skip_program`.
    pub fn used_services(&self, skip_program: u16, skip: &[u16], out: &mut [bool; 256]) {
        let mut mark = |d: &[u8]| {
            each_descriptor(d, |tag, body| {
                if tag == 0x25 || tag == 0x26 {
                    if let Some(id) = metadata_service_id(body) {
                        out[usize::from(id)] = true;
                    }
                }
            })
        };
        for (p, m) in self.pmts() {
            mark(m.info());
            for s in &m.streams {
                if p.number == skip_program && skip.contains(&s.pid) {
                    continue;
                }
                mark(m.desc(s));
            }
        }
    }
}

/// Call `f(tag, body)` for every whole descriptor of a loop.
pub(super) fn each_descriptor(mut d: &[u8], mut f: impl FnMut(u8, &[u8])) {
    while d.len() >= 2 {
        let len = usize::from(d[1]);
        let Some(body) = d.get(2..2 + len) else {
            return;
        };
        f(d[0], body);
        d = &d[2 + len..];
    }
}

/// CA_PIDs of the CA descriptors (tag 0x09) in a loop.
fn ca_pids(d: &[u8], out: &mut Pids) {
    each_descriptor(d, |tag, body| {
        if tag == 0x09 && body.len() >= 4 {
            out.set(u16::from_be_bytes([body[2] & 0x1F, body[3]]));
        }
    });
}
