//! Errors from the write primitives.

use std::fmt;

/// Why a write primitive refused a request. Nothing is written and no
/// state (continuity counter, sequence number, table, version) changes
/// when one of these is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MuxError {
    /// The sink or buffer cannot take everything; `needed` is the total in
    /// bytes (a multiple of 188 for packets).
    BufferTooSmall {
        /// Bytes the write needs.
        needed: usize,
    },
    /// A PID outside the range its use allows (ISO/IEC 13818-1 Table 2-3).
    InvalidPid {
        /// The PID.
        pid: u16,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// Two uses of one PID that cannot share it, such as two elementary
    /// streams (R-ISO13818-020) or a stream on a PMT PID.
    PidConflict {
        /// The PID.
        pid: u16,
        /// What it collides with.
        reason: &'static str,
    },
    /// A program_number that is 0, already present, or otherwise refused.
    InvalidProgram {
        /// The program_number.
        number: u16,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// No program with this program_number.
    UnknownProgram(u16),
    /// No stream on this PID in the program.
    UnknownStream {
        /// The program_number.
        program: u16,
        /// The elementary PID.
        pid: u16,
    },
    /// A section would be longer than the standard allows: section_length
    /// at most 1021 for PAT and PMT (R-ISO13818-080, R-ISO13818-091).
    SectionTooLong {
        /// table_id of the section.
        table_id: u8,
        /// The section_length it would need.
        section_length: usize,
    },
    /// A descriptor longer than 255 bytes (R-ISO13818-105), or a raw
    /// descriptor loop whose lengths do not add up.
    InvalidDescriptor(&'static str),
    /// A section handed to [`TsWriter::write_section`](crate::mux::TsWriter::write_section)
    /// whose length does not match its section_length field.
    InvalidSection(&'static str),
    /// A unit that breaks a rule of its stream's carriage, such as a sync
    /// KLV access unit without a PTS.
    InvalidUnit(&'static str),
    /// A field out of its range, such as a 6-bit version number.
    InvalidValue {
        /// The field.
        field: &'static str,
        /// The rule it breaks.
        reason: &'static str,
    },
}

impl fmt::Display for MuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MuxError::BufferTooSmall { needed } => write!(f, "output buffer too small: {needed} bytes needed"),
            MuxError::InvalidPid { pid, reason } => write!(f, "PID 0x{pid:04X}: {reason}"),
            MuxError::PidConflict { pid, reason } => write!(f, "PID 0x{pid:04X} already in use: {reason}"),
            MuxError::InvalidProgram { number, reason } => write!(f, "program {number}: {reason}"),
            MuxError::UnknownProgram(n) => write!(f, "no program {n}"),
            MuxError::UnknownStream { program, pid } => write!(f, "program {program} has no stream on PID 0x{pid:04X}"),
            MuxError::SectionTooLong { table_id, section_length } => write!(
                f,
                "table 0x{table_id:02X} would need section_length {section_length}, over the limit of 1021"
            ),
            MuxError::InvalidDescriptor(why) => write!(f, "invalid descriptor: {why}"),
            MuxError::InvalidSection(why) => write!(f, "invalid section: {why}"),
            MuxError::InvalidUnit(why) => write!(f, "invalid unit: {why}"),
            MuxError::InvalidValue { field, reason } => write!(f, "invalid {field}: {reason}"),
        }
    }
}

impl std::error::Error for MuxError {}
