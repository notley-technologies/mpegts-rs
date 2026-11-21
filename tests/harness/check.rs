//! A conformance checker for injected streams (MUXING_PLAN.md section 7.3),
//! independent of the injector's own bookkeeping: it reads the output
//! back, times every packet by PCR interpolation (ISO/IEC 13818-1
//! 2.4.2.2) and checks the rules the injector promises.

use mpegts::{crc32_mpeg2, pts_delta, PacketHeader, PcrTimeline, PesHeader};
use std::collections::HashMap;

/// What the output should look like.
#[derive(Debug, Clone)]
pub struct Expect {
    pub program: u16,
    pub pmt_pid: u16,
    pub pcr_pid: Option<u16>,
    pub klv_pid: u16,
    pub sync: bool,
    pub service_id: Option<u8>,
    pub psi_interval_ms: u64,
    pub min_lead_ms: u64,
    pub max_lead_ms: u64,
    /// The KLV PID also carries the input's access units (merge).
    pub merged: bool,
}

/// A KLV unit as it arrived.
#[derive(Debug, Clone)]
pub struct Arrived {
    pub pts: Option<u64>,
    /// Program clock of its first and last packet, 27 MHz.
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub service_id: Option<u8>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Checked {
    /// Every KLV PES on the KLV PID.
    pub units: Vec<Arrived>,
    /// Largest gap between PAT copies and between target PMT copies,
    /// 27 MHz.
    pub pat_gap: u64,
    pub pmt_gap: u64,
    /// The last KLV PES is cut off by the end of the stream.
    pub cut_tail: bool,
    pub errors: Vec<String>,
}

struct Pes {
    bytes: Vec<u8>,
    first: Option<u64>,
    last: Option<u64>,
}

fn sections_of(payload: &[u8], pusi: bool, buf: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
    let mut data = payload;
    if pusi {
        let ptr = usize::from(data[0]);
        if ptr + 1 > data.len() {
            buf.clear();
            return;
        }
        buf.extend_from_slice(&data[1..1 + ptr]);
        drain(buf, out);
        buf.clear();
        data = &data[1 + ptr..];
    }
    buf.extend_from_slice(data);
    drain(buf, out);
}

fn drain(buf: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
    loop {
        if buf.len() < 3 || buf[0] == 0xFF {
            if buf.first() == Some(&0xFF) {
                buf.clear();
            }
            return;
        }
        let len = 3 + ((usize::from(buf[1] & 0x0F) << 8) | usize::from(buf[2]));
        if buf.len() < len {
            return;
        }
        out.push(buf[..len].to_vec());
        buf.drain(..len);
    }
}

/// Check an injected stream.
pub fn check(ts: &[u8], e: &Expect) -> Checked {
    let mut c = Checked::default();
    let err = |c: &mut Checked, m: String| {
        if c.errors.len() < 50 {
            c.errors.push(m)
        }
    };
    if ts.len() % 188 != 0 {
        err(&mut c, "output is not whole packets".into());
        return c;
    }
    // Times by PCR interpolation over the output itself.
    let mut timeline = PcrTimeline::new(e.pcr_pid, 10_000_000).with_max_held(1 << 20);
    let mut timed = Vec::new();
    for p in ts.chunks(188) {
        timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
    }
    timeline.flush(&mut |t| timed.push(t));

    let mut last_cc: HashMap<u16, (u8, [u8; 188])> = HashMap::new();
    let mut psi_buf: HashMap<u16, Vec<u8>> = HashMap::new();
    let mut last_pat: Option<u64> = None;
    let mut last_pmt: Option<u64> = None;
    let mut pes: Option<Pes> = None;
    let mut pes_list: Vec<Pes> = Vec::new();
    let mut pmt_checked = false;
    for (i, t) in timed.iter().enumerate() {
        let p = &t.packet;
        let h = match PacketHeader::parse(p) {
            Ok(h) => h,
            Err(x) => {
                err(&mut c, format!("packet {i}: {x}"));
                continue;
            }
        };
        if h.is_null() {
            continue;
        }
        // Continuity (R-ISO13818-009 to R-ISO13818-013).
        if !h.transport_error {
            if let Some(&(prev, ref bytes)) = last_cc.get(&h.pid) {
                let ok = if h.discontinuity() {
                    true
                } else if h.has_payload() {
                    h.continuity_counter == (prev + 1) & 15
                        || (h.continuity_counter == prev && bytes == p)
                } else {
                    h.continuity_counter == prev
                };
                if !ok {
                    err(
                        &mut c,
                        format!(
                            "packet {i}: CC {} after {prev} on PID 0x{:04X}",
                            h.continuity_counter, h.pid
                        ),
                    );
                }
            }
            last_cc.insert(h.pid, (h.continuity_counter, *p));
        }
        if h.pid == e.klv_pid && h.pcr().is_some() {
            err(
                &mut c,
                format!("packet {i}: PCR on the KLV PID (DECISIONS section 2)"),
            );
        }
        let pos = t.position;
        if (h.pid == 0 || h.pid == e.pmt_pid) && h.has_payload() {
            let mut out = Vec::new();
            let buf = psi_buf.entry(h.pid).or_default();
            sections_of(h.payload(p), h.payload_unit_start, buf, &mut out);
            for s in out {
                if crc32_mpeg2(&s) != 0 {
                    err(
                        &mut c,
                        format!("packet {i}: section with a bad CRC on PID 0x{:04X}", h.pid),
                    );
                    continue;
                }
                if h.pid == 0 && s[0] == 0 {
                    if let Some(l) = last_pat {
                        c.pat_gap = c.pat_gap.max(pos - l);
                    }
                    last_pat = Some(pos);
                }
                if h.pid == e.pmt_pid
                    && s[0] == 2
                    && u16::from_be_bytes([s[3], s[4]]) == e.program
                    && s[5] & 1 == 1
                {
                    if let Some(l) = last_pmt {
                        c.pmt_gap = c.pmt_gap.max(pos - l);
                    }
                    last_pmt = Some(pos);
                    if !pmt_checked {
                        pmt_checked = true;
                        if let Err(m) = check_pmt(&s, e) {
                            err(&mut c, format!("packet {i}: PMT: {m}"));
                        }
                    }
                }
            }
        }
        if h.pid == e.klv_pid && h.has_payload() && !h.transport_error {
            if h.payload_unit_start {
                if let Some(done) = pes.take() {
                    pes_list.push(done);
                }
                pes = Some(Pes {
                    bytes: Vec::new(),
                    first: t.clock,
                    last: t.clock,
                });
            }
            if let Some(cur) = &mut pes {
                cur.bytes.extend_from_slice(h.payload(p));
                cur.last = t.clock;
            }
        }
    }
    if let Some(done) = pes.take() {
        pes_list.push(done);
    }
    let interval = e.psi_interval_ms * 27_000;
    if c.pat_gap > interval {
        let m = format!(
            "PAT gap {} ms over {} ms",
            c.pat_gap / 27_000,
            e.psi_interval_ms
        );
        err(&mut c, m);
    }
    if c.pmt_gap > interval {
        let m = format!(
            "PMT gap {} ms over {} ms",
            c.pmt_gap / 27_000,
            e.psi_interval_ms
        );
        err(&mut c, m);
    }
    if last_pat.is_none() || last_pmt.is_none() {
        err(&mut c, "no PAT or no target PMT in the output".into());
    }

    // PES rules for 0x15 / 0x06 (ST 1402.2, iso13818.md 3.8, 3.9).
    let mut last_pts: Option<u64> = None;
    for (k, p) in pes_list.iter().enumerate() {
        let h = match PesHeader::parse(&p.bytes) {
            Ok(h) => h,
            Err(x) => {
                err(&mut c, format!("KLV PES {k}: {x}"));
                continue;
            }
        };
        if h.packet_length == 0 {
            err(
                &mut c,
                format!("KLV PES {k}: PES_packet_length 0 (R-ISO13818-051)"),
            );
            continue;
        }
        let total = 6 + usize::from(h.packet_length);
        if p.bytes.len() < total && k + 1 == pes_list.len() {
            c.cut_tail = true;
            continue;
        }
        if p.bytes.len() != total {
            err(
                &mut c,
                format!(
                    "KLV PES {k}: {} bytes, PES_packet_length says {total}",
                    p.bytes.len()
                ),
            );
            continue;
        }
        let body = &p.bytes[h.payload_offset..];
        let mut arrived = Arrived {
            pts: h.pts,
            first: p.first,
            last: p.last,
            service_id: None,
            payload: Vec::new(),
        };
        if e.sync {
            if h.stream_id != 0xFC
                || h.pts.is_none()
                || h.dts.is_some()
                || h.header_data_length != 5
                || !h.markers_ok
            {
                err(
                    &mut c,
                    format!("KLV PES {k}: not a sync KLV header (R-ST1402TIME-16, -17)"),
                );
            }
            // Whole cells only (R-ST1402TIME-18).
            let mut at = 0;
            let mut first = true;
            while at < body.len() {
                if body.len() - at < 5 {
                    err(&mut c, format!("KLV PES {k}: cell header cut"));
                    break;
                }
                let len = usize::from(u16::from_be_bytes([body[at + 3], body[at + 4]]));
                if at + 5 + len > body.len() {
                    err(&mut c, format!("KLV PES {k}: cell overruns the PES"));
                    break;
                }
                let starts_au = body[at + 2] >> 6 >= 2;
                if first && h.data_alignment != starts_au {
                    err(
                        &mut c,
                        format!("KLV PES {k}: data_alignment_indicator (DECISIONS X-6)"),
                    );
                }
                if first {
                    arrived.service_id = Some(body[at]);
                }
                first = false;
                if body[at + 2] & 0x0F != 0x0F {
                    err(&mut c, format!("KLV PES {k}: cell reserved bits"));
                }
                arrived
                    .payload
                    .extend_from_slice(&body[at + 5..at + 5 + len]);
                at += 5 + len;
            }
            let ours = !e.merged || arrived.service_id == e.service_id;
            if let (Some(pts), true) = (h.pts, ours) {
                // Non-decreasing PTS per stream, per service when several
                // share it (R-ISO13818-160).
                if let Some(l) = last_pts {
                    if pts_delta(pts, l) < 0 && pts_delta(pts, l) > -(90_000 * 5) {
                        err(&mut c, format!("KLV PES {k}: PTS {pts} after {l}"));
                    }
                }
                last_pts = Some(pts);
                // Arrival window (D-ISO13818-14, R-ISO13818-157), for our
                // own service.
                if let (Some(first), Some(last)) = (p.first, p.last) {
                    let lead_first = pts_delta(pts, (first / 300) % (1 << 33)) * 300;
                    let lead_last = pts_delta(pts, (last / 300) % (1 << 33)) * 300;
                    if lead_first > (e.max_lead_ms * 27_000) as i64 + 300 {
                        err(
                            &mut c,
                            format!(
                                "KLV PES {k}: first byte {} ms before its PTS",
                                lead_first / 27_000
                            ),
                        );
                    }
                    if lead_last < (e.min_lead_ms * 27_000) as i64 - 300 {
                        err(
                            &mut c,
                            format!(
                                "KLV PES {k}: last byte {} us before its PTS",
                                lead_last / 27
                            ),
                        );
                    }
                }
            }
        } else {
            if h.stream_id != 0xBD || h.pts.is_some() || h.header_data_length != 0 {
                err(
                    &mut c,
                    format!("KLV PES {k}: not an async KLV header (R-ST1402TIME-40, -42)"),
                );
            }
            arrived.payload.extend_from_slice(body);
        }
        c.units.push(arrived);
    }
    c
}

/// The target PMT lists the KLV stream with the right descriptors.
fn check_pmt(s: &[u8], e: &Expect) -> Result<(), String> {
    let end = s.len() - 4;
    let info = (usize::from(s[10] & 0x0F) << 8) | usize::from(s[11]);
    let mut at = 12 + info;
    let mut found = false;
    while at + 5 <= end {
        let st = s[at];
        let pid = u16::from_be_bytes([s[at + 1] & 0x1F, s[at + 2]]);
        let len = (usize::from(s[at + 3] & 0x0F) << 8) | usize::from(s[at + 4]);
        let d = &s[at + 5..at + 5 + len];
        if pid == e.klv_pid {
            found = true;
            let mut tags = Vec::new();
            let mut services = Vec::new();
            let mut j = 0;
            while j + 2 <= d.len() {
                tags.push(d[j]);
                if d[j] == 0x26 {
                    services.push(d[j + 9]);
                    if d[j + 1] != 9
                        || d[j + 2..j + 9] != [0x01, 0x00, 0xFF, b'K', b'L', b'V', b'A']
                        || d[j + 10] != 0x0F
                    {
                        return Err("metadata_descriptor bytes".into());
                    }
                }
                j += 2 + usize::from(d[j + 1]);
            }
            if e.sync {
                if st != 0x15 {
                    return Err(format!("stream_type 0x{st:02X}"));
                }
                if tags.iter().filter(|&&t| t == 0x27).count() != 1 {
                    return Err("not exactly one metadata_STD_descriptor (R-ST1402TIME-26)".into());
                }
                if let Some(id) = e.service_id {
                    if !services.contains(&id) {
                        return Err(format!("no metadata_descriptor for service {id}"));
                    }
                }
            } else if st != 0x06
                || !d
                    .windows(6)
                    .any(|w| w == [0x05, 0x04, b'K', b'L', b'V', b'A'])
            {
                return Err("async entry needs stream_type 0x06 and the KLVA registration".into());
            }
        }
        at += 5 + len;
    }
    if found {
        Ok(())
    } else {
        Err(format!("no entry for PID 0x{:04X}", e.klv_pid))
    }
}
