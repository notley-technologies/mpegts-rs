//! Deciding what the injector does with the input's current tables: the
//! target program, the KLV PID and metadata_service_id, the streams it
//! removes or merges into, and the rewritten PMT.

use super::super::spec::{
    klv_metadata_descriptor, metadata_std_descriptor, KLVA_REGISTRATION_DESCRIPTOR,
};
use super::tables::{each_descriptor, Pids, Pmt, Tables};
use super::{ExistingKlv, InjectConfig, InjectError, KlvCarriage, ProgramSelect, Target};
use crate::codec::Codec;
use crate::crc32_mpeg2;
use crate::packet::NULL_PID;

/// Most section_length a PMT may have (R-ISO13818-091).
const MAX_SECTION_LENGTH: usize = 1021;
/// The ATSC base PID, kept free by convention (D-ISO13818-11).
const ATSC_BASE_PID: u16 = 0x1FFB;

/// How the target PMT is rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rewrite {
    /// ES entries removed (ExistingKlv::Replace).
    pub removed: Vec<u16>,
    /// The existing synchronous KLV PID our service joins, with our
    /// metadata_descriptor (ExistingKlv::Keep, st1402-time.md D-5).
    pub merged: Option<(u16, [u8; 11])>,
    /// Our metadata_STD_descriptor, added to a merged entry that has none
    /// (R-ST1402TIME-26).
    pub std: [u8; 11],
    /// Our own ES entry: stream_type, PID and descriptor loop.
    pub entry: Option<(u8, u16, Vec<u8>)>,
}

/// What the injector does, for the tables it last saw.
#[derive(Debug, Clone)]
pub(super) struct Plan {
    pub target: Target,
    /// The input target PMT section this plan was made from; empty while
    /// the program's PMT on a new PMT PID has not arrived.
    pub pmt_in: Vec<u8>,
    /// Its rewrite, the section the injector sends.
    pub pmt_out: Option<Vec<u8>>,
    pub rewrite: Rewrite,
    /// Version of `pmt_in`.
    pub input_version: u8,
    /// PIDs whose packets are dropped.
    pub strip: Vec<u16>,
    /// Elementary PIDs of the target program.
    pub es_pids: Vec<u16>,
    /// Programs in the PAT.
    pub programs: usize,
    /// Size of the KLV stream's metadata buffer B_n in bytes: from the
    /// existing stream's metadata_STD_descriptor when joining one, else
    /// ours (R-ISO13818-159).
    pub bn_capacity: u64,
}

pub(super) struct PlanInput<'a> {
    pub cfg: &'a InjectConfig,
    pub tables: &'a Tables,
    /// PIDs seen in the input so far.
    pub observed: &'a Pids,
    pub previous: Option<&'a Plan>,
    /// Our own PMT changes since the start (D-ISO13818-12).
    pub own_changes: u8,
    /// metadata_service_ids seen in the cells of the merged stream.
    pub cell_services: &'a [bool; 256],
}

/// The plan for the current tables: `None` when no target program can be
/// chosen yet.
pub(super) fn plan(i: &PlanInput<'_>) -> Result<Option<Plan>, InjectError> {
    let t = i.tables;
    if t.pat.is_none() {
        return Ok(i.previous.cloned());
    }
    let prev_target = i.previous.map(|p| p.target.program);
    let number = match i.cfg.program {
        ProgramSelect::Number(n) => n,
        ProgramSelect::Auto => match prev_target.filter(|&n| t.program(n).is_some()) {
            Some(n) => n,
            // The first program with video, else the first program, among
            // those whose PMT is known.
            None => match t
                .pmts()
                .find(|(_, m)| m.video().is_some())
                .or_else(|| t.pmts().next())
            {
                Some((p, _)) => p.number,
                None => return Ok(None),
            },
        },
    };
    let Some(program) = t.program(number) else {
        return Ok(None);
    };
    let Some(pmt) = program.pmt.as_ref() else {
        // The PMT moved to another PID and has not arrived there yet: keep
        // injecting as before, without PMT copies.
        return Ok(i.previous.filter(|p| p.target.program == number).map(|p| {
            let mut p = p.clone();
            p.target.pmt_pid = program.pmt_pid;
            p.pmt_in.clear();
            p.pmt_out = None;
            p
        }));
    };
    let mut p = build(i, number, program.pmt_pid, pmt, i.own_changes)?;
    // Our part of the PMT changed while the input's did not: that is a new
    // version too (D-ISO13818-12, R-ISO13818-093).
    if let Some(prev) = i.previous {
        if prev.target.program == number
            && prev.input_version == p.input_version
            && prev.pmt_in == p.pmt_in
            && prev.rewrite != p.rewrite
        {
            p = build(
                i,
                number,
                program.pmt_pid,
                pmt,
                i.own_changes.wrapping_add(1),
            )?;
        }
    }
    Ok(Some(p))
}

fn build(
    i: &PlanInput<'_>,
    number: u16,
    pmt_pid: u16,
    pmt: &Pmt,
    own_changes: u8,
) -> Result<Plan, InjectError> {
    let cfg = i.cfg;
    let t = i.tables;
    let prev = i.previous.filter(|p| p.target.program == number);
    let pcr_pid = pmt.pcr();
    let video_pid = pmt.video().map(|s| s.pid);
    // Elementary PIDs are 0x0010 to 0x1FFE (R-ISO13818-015); an entry
    // elsewhere is not a stream the injector can remove or join.
    let valid = |pid: u16| (0x0010..NULL_PID).contains(&pid);
    let klv: Vec<(u16, Codec)> = pmt
        .streams
        .iter()
        .filter(|s| s.codec.is_klv() && valid(s.pid))
        .map(|s| (s.pid, s.codec))
        .collect();

    // Existing KLV in the target program (st1402-time.md D-5).
    let mut removed = Vec::new();
    let mut merged = None;
    match cfg.existing_klv {
        ExistingKlv::Fail => {
            if let Some(&(pid, _)) = klv.first() {
                return Err(InjectError::ExistingKlv {
                    program: number,
                    pid,
                });
            }
        }
        ExistingKlv::Replace => removed = klv.iter().map(|&(pid, _)| pid).collect(),
        ExistingKlv::Keep => {
            // More synchronous metadata joins the existing synchronous
            // stream as another service, never a second 0x15 stream
            // (R-ST1402TIME-23, ST 1402-13).
            if cfg.carriage == KlvCarriage::Sync {
                merged = pmt
                    .streams
                    .iter()
                    .find(|s| s.codec == Codec::KlvSync && valid(s.pid))
                    .map(|s| s.pid);
            }
        }
    }
    if let (Some(pid), Some(fixed)) = (merged, cfg.pid) {
        if pid != fixed {
            return Err(InjectError::PidInUse {
                pid: fixed,
                reason: "the program already carries synchronous KLV, which ExistingKlv::Keep joins on its own PID (ST 1402-13)",
            });
        }
    }
    if merged.is_some() && merged == pcr_pid {
        return Err(InjectError::Unsupported(
            "the existing synchronous KLV stream carries the program's PCR; use ExistingKlv::Replace",
        ));
    }

    // Removed streams listed by another program still flow for it: only
    // the entry goes, not the packets (R-ISO13818-020).
    let shared = |pid: u16| {
        t.pmts()
            .any(|(p, m)| p.number != number && m.streams.iter().any(|s| s.pid == pid))
    };
    let strip: Vec<u16> = removed
        .iter()
        .copied()
        .filter(|&pid| !shared(pid))
        .collect();

    let mut used = Pids::default();
    t.used_pids(&mut used);
    let table_used = used.clone();
    for pid in 0..=0x1FFF {
        if i.observed.get(pid) {
            used.set(pid);
        }
    }
    let highest = pmt.streams.iter().map(|s| s.pid).max().unwrap_or(0x001F);
    // A removed stream's PID may carry ours unless it carries the PCR
    // (DECISIONS section 2: the KLV PID never carries PCR), or another
    // program or a CA descriptor still names it.
    let reusable = |pid: u16| {
        strip.contains(&pid)
            && Some(pid) != pcr_pid
            && !t.cat_pids.contains(&pid)
            && !ca_named(t, pid)
            && pid != pmt_pid
    };
    let klv_pid = match merged {
        Some(pid) => pid,
        None => match cfg.pid {
            Some(pid) => {
                if Some(pid) == pcr_pid {
                    return Err(InjectError::PidInUse {
                        pid,
                        reason: "it is the program's PCR_PID",
                    });
                }
                if used.get(pid) && !reusable(pid) {
                    return Err(InjectError::PidInUse {
                        pid,
                        reason: "the input uses it",
                    });
                }
                pid
            }
            None => {
                let auto_legal = |pid: u16| {
                    (0x0020..NULL_PID).contains(&pid)
                        && pid != ATSC_BASE_PID
                        && pid & 0xFF != 0x47
                        && Some(pid) != pcr_pid
                        && (!used.get(pid) || reusable(pid))
                };
                match prev.map(|p| p.target.klv_pid) {
                    // Keep our PID while the input's tables leave it to us;
                    // a later input that takes it is a collision, never a
                    // silent move (D-ISO13818-11).
                    Some(pid) => {
                        if (table_used.get(pid) || Some(pid) == pcr_pid) && !reusable(pid) {
                            return Err(InjectError::PidInUse {
                                pid,
                                reason: "the input started using the KLV PID",
                            });
                        }
                        pid
                    }
                    None => match removed.iter().copied().find(|&pid| reusable(pid)) {
                        Some(pid) => pid,
                        None => (highest.saturating_add(1)..NULL_PID)
                            .chain(0x0020..NULL_PID)
                            .find(|&pid| auto_legal(pid))
                            .ok_or(InjectError::NoFreePid)?,
                    },
                }
            }
        },
    };

    // metadata_service_id, unique in the whole transport stream
    // (D-ISO13818-03, R-ST1402TIME-35).
    let service_id = match cfg.carriage {
        KlvCarriage::Async => None,
        KlvCarriage::Sync => {
            let mut taken = [false; 256];
            t.used_services(number, &removed, &mut taken);
            if merged.is_some() {
                for (id, seen) in i.cell_services.iter().enumerate() {
                    taken[id] |= seen;
                }
            }
            let old = removed_service(pmt, klv_pid, &removed);
            Some(match cfg.service_id {
                Some(id) if taken[usize::from(id)] => return Err(InjectError::ServiceIdInUse(id)),
                Some(id) => id,
                None => match prev
                    .and_then(|p| p.target.service_id)
                    .filter(|&id| !taken[usize::from(id)])
                {
                    Some(id) => id,
                    None => match old.filter(|&id| !taken[usize::from(id)]) {
                        Some(id) => id,
                        None => (0..=255u8)
                            .find(|&id| !taken[usize::from(id)])
                            .ok_or(InjectError::NoFreeServiceId)?,
                    },
                },
            })
        }
    };

    let std = metadata_std_descriptor(
        cfg.sync.input_leak_rate,
        cfg.sync.buffer_size,
        cfg.sync.output_leak_rate,
    )
    .map_err(|_| {
        InjectError::InvalidConfig("metadata_STD_descriptor fields must fit in 22 bits")
    })?;
    let rewrite = Rewrite {
        removed: removed.clone(),
        merged: merged.map(|pid| (pid, klv_metadata_descriptor(service_id.unwrap_or(0)))),
        std,
        entry: match (merged, cfg.carriage) {
            (Some(_), _) => None,
            (None, KlvCarriage::Sync) => {
                // metadata_descriptor, metadata_STD_descriptor, then the
                // optional registration descriptor (st1402-time.md D-1 and
                // 3.5, DECISIONS X-1, X-2).
                let mut d = klv_metadata_descriptor(service_id.unwrap_or(0)).to_vec();
                d.extend_from_slice(&std);
                if cfg.sync.sync_registration_descriptor {
                    d.extend_from_slice(&KLVA_REGISTRATION_DESCRIPTOR);
                }
                Some((0x15, klv_pid, d))
            }
            // stream_type 0x06 with the "KLVA" registration descriptor
            // (R-ST1402TIME-40, R-ST1402TIME-44).
            (None, KlvCarriage::Async) => {
                Some((0x06, klv_pid, KLVA_REGISTRATION_DESCRIPTOR.to_vec()))
            }
        },
    };
    let version = (pmt.version + 1).wrapping_add(own_changes) & 0x1F;
    let pmt_out = rewrite_pmt(&pmt.raw, &rewrite, version)?;
    let mut bn_capacity = u64::from(cfg.sync.buffer_size) * 1024;
    if let Some(s) = merged.and_then(|pid| pmt.streams.iter().find(|s| s.pid == pid)) {
        each_descriptor(pmt.desc(s), |tag, b| {
            if tag == 0x27 && b.len() >= 6 {
                // metadata_buffer_size: 22 bits after '11', units of 1024
                // bytes (R-ISO13818-126).
                let size =
                    (u64::from(b[3] & 0x3F) << 16) | (u64::from(b[4]) << 8) | u64::from(b[5]);
                bn_capacity = size * 1024;
            }
        });
    }
    Ok(Plan {
        target: Target {
            program: number,
            pmt_pid,
            pcr_pid,
            video_pid,
            klv_pid,
            service_id,
            carriage: cfg.carriage,
            merged: merged.is_some(),
            stripped: strip.clone(),
        },
        pmt_in: pmt.raw.clone(),
        pmt_out: Some(pmt_out),
        rewrite,
        input_version: pmt.version,
        strip,
        es_pids: pmt.streams.iter().map(|s| s.pid).collect(),
        programs: t.programs.len(),
        bn_capacity,
    })
}

/// A CA descriptor of any PMT names `pid`.
fn ca_named(t: &Tables, pid: u16) -> bool {
    let mut found = false;
    for (_, m) in t.pmts() {
        let mut check = |d: &[u8]| {
            each_descriptor(d, |tag, b| {
                if tag == 0x09 && b.len() >= 4 && u16::from_be_bytes([b[2] & 0x1F, b[3]]) == pid {
                    found = true;
                }
            })
        };
        check(m.info());
        for s in &m.streams {
            check(m.desc(s));
        }
    }
    found
}

/// The metadata_service_id of the removed synchronous stream on `pid`,
/// reused when our stream takes over its PID (D-ISO13818-03).
fn removed_service(pmt: &Pmt, pid: u16, removed: &[u16]) -> Option<u8> {
    if !removed.contains(&pid) {
        return None;
    }
    let s = pmt
        .streams
        .iter()
        .find(|s| s.pid == pid && s.codec == Codec::KlvSync)?;
    let mut id = None;
    each_descriptor(pmt.desc(s), |tag, b| {
        if tag == 0x26 && id.is_none() {
            id = super::super::spec::metadata_service_id(b);
        }
    });
    id
}

/// Rewrite a target PMT section (D-ISO13818-12): version `version`, the
/// input's current_next_indicator, PCR_PID and program_info kept, removed
/// entries dropped, our metadata_descriptor added to a merged entry, our
/// own entry appended, CRC recomputed (R-ISO13818-100). Every other byte
/// of the input's entries is copied. Fails when the result would exceed
/// section_length 1021 (R-ISO13818-091, R-ISO13818-092).
pub(super) fn rewrite_pmt(raw: &[u8], r: &Rewrite, version: u8) -> Result<Vec<u8>, InjectError> {
    let end = raw
        .len()
        .checked_sub(4)
        .filter(|&e| e >= 12)
        .ok_or(InjectError::Unsupported("malformed PMT section"))?;
    let info_len = (usize::from(raw[10] & 0x0F) << 8) | usize::from(raw[11]);
    let mut at = 12 + info_len;
    if at > end {
        return Err(InjectError::Unsupported("malformed PMT section"));
    }
    let mut out = Vec::with_capacity(raw.len() + 32);
    out.extend_from_slice(&raw[..at]);
    while end - at >= 5 {
        let pid = u16::from_be_bytes([raw[at + 1] & 0x1F, raw[at + 2]]);
        let es_len = (usize::from(raw[at + 3] & 0x0F) << 8) | usize::from(raw[at + 4]);
        let next = at + 5 + es_len;
        if next > end {
            return Err(InjectError::Unsupported("malformed PMT section"));
        }
        let desc = &raw[at + 5..next];
        match r.merged {
            _ if r.removed.contains(&pid) => {}
            Some((merged, md)) if merged == pid => {
                // Our metadata_descriptor after the stream's own ones
                // (R-ST1402TIME-25); exactly one metadata_STD_descriptor
                // per stream, so the input's stays and ours is added only
                // when it has none (R-ST1402TIME-26).
                let mut loop_ = Vec::with_capacity(desc.len() + 22);
                let mut last_md = None;
                let mut has_std = false;
                let mut whole = 0;
                each_descriptor(desc, |tag, b| {
                    whole += 2 + b.len();
                    if tag == 0x26 {
                        last_md = Some(whole);
                    }
                    has_std |= tag == 0x27;
                });
                if whole != desc.len() {
                    return Err(InjectError::Unsupported(
                        "malformed ES_info loop on the existing KLV stream",
                    ));
                }
                let split = last_md.unwrap_or(0);
                loop_.extend_from_slice(&desc[..split]);
                loop_.extend_from_slice(&md);
                if !has_std {
                    loop_.extend_from_slice(&r.std);
                }
                loop_.extend_from_slice(&desc[split..]);
                push_entry(&mut out, raw[at], &raw[at + 1..at + 3], &loop_)?;
            }
            _ => out.extend_from_slice(&raw[at..next]),
        }
        at = next;
    }
    if let Some((stream_type, pid, d)) = &r.entry {
        push_entry(&mut out, *stream_type, &(0xE000 | pid).to_be_bytes(), d)?;
    }
    let section_length = out.len() + 4 - 3;
    if section_length > MAX_SECTION_LENGTH {
        return Err(InjectError::SectionTooLong { section_length });
    }
    // section_syntax_indicator 1, '0', reserved '11', section_length
    // (R-ISO13818-079, R-ISO13818-090); version_number with the input's
    // current_next_indicator (R-ISO13818-094, D-ISO13818-12).
    out[1] = 0xB0 | (section_length >> 8) as u8;
    out[2] = section_length as u8;
    out[5] = 0xC0 | (version & 0x1F) << 1 | (raw[5] & 1);
    let crc = crc32_mpeg2(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(out)
}

fn push_entry(out: &mut Vec<u8>, stream_type: u8, pid: &[u8], d: &[u8]) -> Result<(), InjectError> {
    if d.len() > 0x3FF {
        return Err(InjectError::SectionTooLong {
            section_length: out.len() + 5 + d.len() + 1,
        });
    }
    out.push(stream_type);
    out.extend_from_slice(pid);
    // reserved '1111' + ES_info_length ('00' prefix, R-ISO13818-097).
    out.extend_from_slice(&(0xF000 | d.len() as u16).to_be_bytes());
    out.extend_from_slice(d);
    Ok(())
}
