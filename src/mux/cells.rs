//! Metadata access unit cells (ISO/IEC 13818-1 2.12.4.1, Tables 2-108 and
//! 2-109), the wrapper of synchronous metadata in PES packets.

/// Bytes of a cell header (R-ISO13818-139).
pub const CELL_HEADER_LEN: usize = 5;

/// Most access unit bytes in one cell when, as this crate writes them,
/// each cell is alone in a PES packet with a PTS: 65535 (largest
/// PES_packet_length) - 3 (flags and PES_header_data_length) - 5 (PTS) - 5
/// (cell header) = 65,522 (iso13818.md 3.12, DECISIONS section 2).
pub const MAX_CELL_DATA: usize = 65_522;

/// cell_fragment_indication (Table 2-109, R-ISO13818-141).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellFragment {
    /// '11': the cell carries a complete access unit.
    Complete,
    /// '10': the first cell of a fragmented access unit.
    First,
    /// '00': a cell between the first and the last.
    Middle,
    /// '01': the last cell of a fragmented access unit.
    Last,
}

impl CellFragment {
    /// The 2-bit code.
    pub fn code(self) -> u8 {
        match self {
            CellFragment::Complete => 0b11,
            CellFragment::First => 0b10,
            CellFragment::Middle => 0b00,
            CellFragment::Last => 0b01,
        }
    }

    /// The fragment kind for a 2-bit code (higher bits are ignored).
    pub fn from_code(code: u8) -> CellFragment {
        match code & 0b11 {
            0b11 => CellFragment::Complete,
            0b10 => CellFragment::First,
            0b00 => CellFragment::Middle,
            _ => CellFragment::Last,
        }
    }

    /// The kind of cell `index` of `count` cells holding one access unit.
    pub fn of(index: usize, count: usize) -> CellFragment {
        match (index, count) {
            (_, 0 | 1) => CellFragment::Complete,
            (0, _) => CellFragment::First,
            (i, n) if i + 1 >= n => CellFragment::Last,
            _ => CellFragment::Middle,
        }
    }
}

/// A metadata_AU_cell header (Table 2-108).
///
/// ```
/// use mpegts::mux::{CellFragment, CellHeader};
///
/// let h = CellHeader::new(0, 7, CellFragment::Complete, 200);
/// assert_eq!(h.to_bytes(), [0x00, 0x07, 0xDF, 0x00, 0xC8]);
/// assert_eq!(CellHeader::parse(&h.to_bytes()), Some(h));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CellHeader {
    /// metadata_service_id, equal to the service's metadata_descriptor
    /// (R-ISO13818-146).
    pub service_id: u8,
    /// sequence_number: from 0 at the start of each access unit, +1 per
    /// cell, whatever the service (R-ISO13818-140, DECISIONS X-8); see
    /// [`CellNumbering`].
    pub sequence_number: u8,
    /// cell_fragment_indication.
    pub fragment: CellFragment,
    /// decoder_config_flag: the access unit carries decoder configuration
    /// (never for KLV, R-ST1402TIME-30).
    pub decoder_config: bool,
    /// random_access_indicator: decoding can start at this cell
    /// (R-ISO13818-142).
    pub random_access: bool,
    /// AU_cell_data_length: data bytes after the header (R-ISO13818-144).
    pub data_length: u16,
}

impl CellHeader {
    /// A header with the values written for KLV: decoder_config_flag 0 and
    /// random_access_indicator 1 exactly on complete and first cells, since
    /// every KLV packet decodes on its own and middle and last fragments
    /// cannot be entry points (R-ST1402TIME-32, D-ISO13818-07). Byte 2 is
    /// then 0xDF, 0x9F, 0x0F or 0x4F (DECISIONS section 2).
    pub fn new(service_id: u8, sequence_number: u8, fragment: CellFragment, data_length: u16) -> Self {
        CellHeader {
            service_id,
            sequence_number,
            fragment,
            decoder_config: false,
            random_access: matches!(fragment, CellFragment::Complete | CellFragment::First),
            data_length,
        }
    }

    /// The 5 header bytes. The 4 reserved bits are '1111'
    /// (R-ISO13818-103).
    pub fn to_bytes(&self) -> [u8; CELL_HEADER_LEN] {
        let len = self.data_length.to_be_bytes();
        [
            self.service_id,
            self.sequence_number,
            self.fragment.code() << 6 | u8::from(self.decoder_config) << 5 | u8::from(self.random_access) << 4 | 0x0F,
            len[0],
            len[1],
        ]
    }

    /// The header at the start of `b`, if there are 5 bytes. The reserved
    /// bits are not checked.
    pub fn parse(b: &[u8]) -> Option<CellHeader> {
        let b = b.get(..CELL_HEADER_LEN)?;
        Some(CellHeader {
            service_id: b[0],
            sequence_number: b[1],
            fragment: CellFragment::from_code(b[2] >> 6),
            decoder_config: b[2] & 0x20 != 0,
            random_access: b[2] & 0x10 != 0,
            data_length: u16::from_be_bytes([b[3], b[4]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_bytes_match_the_settled_values() {
        let byte2 = |f| CellHeader::new(0, 0, f, 0).to_bytes()[2];
        assert_eq!(byte2(CellFragment::Complete), 0xDF);
        assert_eq!(byte2(CellFragment::First), 0x9F);
        assert_eq!(byte2(CellFragment::Middle), 0x0F);
        assert_eq!(byte2(CellFragment::Last), 0x4F);
        for f in [CellFragment::Complete, CellFragment::First, CellFragment::Middle, CellFragment::Last] {
            assert_eq!(CellFragment::from_code(f.code()), f);
        }
    }

    #[test]
    fn fragment_of_index() {
        assert_eq!(CellFragment::of(0, 1), CellFragment::Complete);
        assert_eq!(CellFragment::of(0, 3), CellFragment::First);
        assert_eq!(CellFragment::of(1, 3), CellFragment::Middle);
        assert_eq!(CellFragment::of(2, 3), CellFragment::Last);
        assert_eq!(CellFragment::of(1, 2), CellFragment::Last);
    }

    #[test]
    fn parse_round_trip() {
        let h = CellHeader::new(0xA5, 255, CellFragment::Last, 65_522);
        assert_eq!(CellHeader::parse(&h.to_bytes()), Some(h));
        assert_eq!(CellHeader::parse(&[0; 4]), None);
    }
}

/// How the sequence_number of metadata access unit cells is counted.
///
/// ISO/IEC 13818-1 2.12.4.1: "This number increments by one for each
/// successive metadata_AU_cell constituting the metadata_AU_wrapper", the
/// wrapper being the cells of one PES packet. DECISIONS X-8: each access
/// unit numbers its cells from 0. An access unit in one PES packet (every
/// one up to 65,522 bytes) is then cell 0 of its wrapper, which MediaMTX
/// (its `mediacommon` library) requires; the fragments of a larger one,
/// one per PES packet, go on counting 1, 2, ... so a receiver can tell
/// that none was lost and join them (the patched FFmpeg does). The
/// per-stream reading of the first release is kept as an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum CellNumbering {
    /// From 0 at the start of each access unit, +1 per cell (fragment),
    /// across the PES packets that carry it. The default.
    #[default]
    PerAccessUnit,
    /// One counter per PID, +1 per cell, modulo 256, whatever the access
    /// unit or service (the reading of D-ISO13818-07 before X-8).
    PerStream,
}
