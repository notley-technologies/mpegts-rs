//! Where written packets go.

use super::MuxError;
use crate::PACKET_SIZE;

/// One 188-byte transport stream packet (R-ISO13818-001).
pub type Packet = [u8; PACKET_SIZE];

/// Bytes of an M2TS record: a 4-byte TP_extra_header and a packet.
pub const M2TS_RECORD_SIZE: usize = 192;

/// One 192-byte M2TS record (BDAV MPEG-2 transport stream): a 4-byte
/// TP_extra_header (2-bit copy_permission_indicator, 30-bit
/// arrival_time_stamp), then a transport stream packet.
pub type Record = [u8; M2TS_RECORD_SIZE];

/// Receives the packets a writer produces, one whole packet at a time.
///
/// Implemented for closures taking `&Packet`, for `Vec<u8>` (packets are
/// appended) and for [`SliceSink`] (a fixed buffer). A sink with limited
/// room reports it through [`remaining`](Self::remaining): writers check
/// it first and fail with [`MuxError::BufferTooSmall`] instead of
/// emitting part of a unit.
pub trait PacketSink {
    /// Take one packet.
    fn packet(&mut self, packet: &Packet);

    /// How many more packets fit, or `None` for no limit (the default).
    fn remaining(&self) -> Option<usize> {
        None
    }

    /// Take one 192-byte M2TS record. Only an injector configured for
    /// M2TS ([`RecordFormat::M2ts`](super::RecordFormat::M2ts)) calls it,
    /// instead of [`packet`](Self::packet). The default passes the packet
    /// to [`packet`](Self::packet) and drops the TP_extra_header; `Vec<u8>`,
    /// [`SliceSink`] and [`RecordFn`] keep whole records.
    fn record(&mut self, record: &Record) {
        let mut p = [0u8; PACKET_SIZE];
        p.copy_from_slice(&record[M2TS_RECORD_SIZE - PACKET_SIZE..]);
        self.packet(&p);
    }
}

impl<F: FnMut(&Packet)> PacketSink for F {
    fn packet(&mut self, packet: &Packet) {
        self(packet)
    }
}

impl PacketSink for Vec<u8> {
    fn packet(&mut self, packet: &Packet) {
        self.extend_from_slice(packet);
    }

    fn record(&mut self, record: &Record) {
        self.extend_from_slice(record);
    }
}

/// A [`PacketSink`] over a closure taking whole M2TS records, for an
/// injector configured for M2TS. A plain packet (which such an injector
/// never sends) gets a zero TP_extra_header.
///
/// ```
/// use mpegts::mux::{PacketSink, Record, RecordFn, NULL_PACKET};
///
/// let mut n = 0;
/// let mut sink = RecordFn(|r: &Record| n += r.len());
/// sink.packet(&NULL_PACKET);
/// assert_eq!(n, 192);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct RecordFn<F>(pub F);

impl<F: FnMut(&Record)> PacketSink for RecordFn<F> {
    fn packet(&mut self, packet: &Packet) {
        let mut r = [0u8; M2TS_RECORD_SIZE];
        r[M2TS_RECORD_SIZE - PACKET_SIZE..].copy_from_slice(packet);
        (self.0)(&r);
    }

    fn record(&mut self, record: &Record) {
        (self.0)(record);
    }
}

/// A [`PacketSink`] over a caller's byte buffer, for writing without an
/// allocator. Packets (or M2TS records) are stored back to back from the
/// start; [`remaining`](PacketSink::remaining) counts 188-byte packets.
///
/// ```
/// use mpegts::mux::{SliceSink, TsWriter, NULL_PACKET};
///
/// let mut buf = [0u8; 188 * 4];
/// let mut sink = SliceSink::new(&mut buf);
/// let mut writer = TsWriter::new(0x100)?;
/// writer.write_pes(&[&[0, 0, 1, 0xE0, 0, 0, 0x80, 0, 0]], &Default::default(), &mut sink)?;
/// assert_eq!(sink.written().len(), 188);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
#[derive(Debug)]
pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> SliceSink<'a> {
    /// A sink that fills `buf` from the start.
    pub fn new(buf: &'a mut [u8]) -> Self {
        SliceSink { buf, len: 0 }
    }

    /// Bytes written so far (a multiple of 188, or of 192 for records).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Nothing written yet.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The packets written so far.
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl PacketSink for SliceSink<'_> {
    fn packet(&mut self, packet: &Packet) {
        // Writers check `remaining` before emitting, so this always fits;
        // a sink used directly drops what does not.
        if let Some(dst) = self.buf.get_mut(self.len..self.len + PACKET_SIZE) {
            dst.copy_from_slice(packet);
            self.len += PACKET_SIZE;
        }
    }

    fn remaining(&self) -> Option<usize> {
        Some((self.buf.len() - self.len) / PACKET_SIZE)
    }

    fn record(&mut self, record: &Record) {
        if let Some(dst) = self.buf.get_mut(self.len..self.len + M2TS_RECORD_SIZE) {
            dst.copy_from_slice(record);
            self.len += M2TS_RECORD_SIZE;
        }
    }
}

/// Fail before writing when the sink cannot take `packets` packets.
pub(crate) fn check_room<S: PacketSink + ?Sized>(sink: &S, packets: usize) -> Result<(), MuxError> {
    match sink.remaining() {
        Some(room) if room < packets => Err(MuxError::BufferTooSmall { needed: packets * PACKET_SIZE }),
        _ => Ok(()),
    }
}
