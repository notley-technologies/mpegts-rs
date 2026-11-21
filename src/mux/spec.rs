//! Stream descriptions, and the KLV carriage presets of MISB ST 1402.2 and
//! SMPTE RP 217.

use super::{Carriage, CellNumbering, MuxError};
use crate::Descriptor;

/// The SMPTE-registered format identifier of KLV, "KLVA" (0x4B4C5641;
/// R-ISO13818-109, R-ST1402TIME-27).
pub const KLVA: [u8; 4] = *b"KLVA";

/// registration_descriptor with format_identifier "KLVA":
/// `05 04 4B 4C 56 41` (R-ISO13818-108, R-ST1402TIME-44). Required on
/// asynchronous KLV streams; on synchronous ones only with
/// [`SyncKlvOptions::sync_registration_descriptor`] (DECISIONS X-1).
pub const KLVA_REGISTRATION_DESCRIPTOR: [u8; 6] = [0x05, 0x04, KLVA[0], KLVA[1], KLVA[2], KLVA[3]];

/// The default metadata_STD_descriptor, `27 09 FF FF FF C0 04 00 C0 00 00`:
/// metadata_input_leak_rate 0x3FFFFF (the maximum, 1,677,721,200 bit/s),
/// metadata_buffer_size 0x000400 (1 MiB), metadata_output_leak_rate 0
/// (DECISIONS X-2, st1402-time.md D-3).
pub const METADATA_STD_DESCRIPTOR: [u8; 11] = [0x27, 0x09, 0xFF, 0xFF, 0xFF, 0xC0, 0x04, 0x00, 0xC0, 0x00, 0x00];

/// Largest value of the 22-bit metadata_STD_descriptor fields.
const MAX_22_BITS: u32 = (1 << 22) - 1;

/// metadata_descriptor for KLV, `26 09 01 00 FF 4B 4C 56 41 <service> 0F`:
/// metadata_application_format 0x0100 (the only value ST 1402.2 allows,
/// R-ST1402TIME-28), metadata_format 0xFF followed by
/// metadata_format_identifier "KLVA" (R-ST1402TIME-27,
/// R-ISO13818-114, R-ISO13818-115), the metadata_service_id,
/// decoder_config_flags '000', DSM-CC_flag 0 and reserved '1111'
/// (R-ST1402TIME-29, R-ST1402TIME-30, R-ISO13818-121, R-ISO13818-122).
pub fn klv_metadata_descriptor(service_id: u8) -> [u8; 11] {
    [0x26, 0x09, 0x01, 0x00, 0xFF, KLVA[0], KLVA[1], KLVA[2], KLVA[3], service_id, 0x0F]
}

/// metadata_STD_descriptor (ISO/IEC 13818-1 2.6.62, Table 2-89,
/// R-ISO13818-124): three 22-bit fields, each after reserved '11'.
/// `input_leak_rate` is in units of 400 bit/s (R-ISO13818-125),
/// `buffer_size` in units of 1024 bytes (R-ISO13818-126); for synchronous
/// metadata `output_leak_rate` is unspecified and should be 0
/// (R-ISO13818-127, R-ST1402TIME-31).
pub fn metadata_std_descriptor(input_leak_rate: u32, buffer_size: u32, output_leak_rate: u32) -> Result<[u8; 11], MuxError> {
    let mut d = [0x27, 0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for (i, (field, v)) in
        [("metadata_input_leak_rate", input_leak_rate), ("metadata_buffer_size", buffer_size), ("metadata_output_leak_rate", output_leak_rate)]
            .into_iter()
            .enumerate()
    {
        if v > MAX_22_BITS {
            return Err(MuxError::InvalidValue { field, reason: "must fit in 22 bits (at most 0x3FFFFF)" });
        }
        d[2 + 3 * i] = 0xC0 | (v >> 16) as u8;
        d[3 + 3 * i] = (v >> 8) as u8;
        d[4 + 3 * i] = v as u8;
    }
    Ok(d)
}

/// metadata_pointer_descriptor for a KLV service carried in the same
/// transport stream (ISO/IEC 13818-1 2.6.58, Table 2-84):
/// `25 0B 01 00 FF 4B 4C 56 41 <service> 1F <program_number>`, that is no
/// locator record and MPEG_carriage_flags '00' (R-ISO13818-112,
/// R-ISO13818-116, R-ISO13818-117). Not required by ST 1402.2 and not
/// written by default (R-ST1402TIME-38, st1402-time.md D-2); add it to a
/// program's descriptors when wanted.
pub fn klv_metadata_pointer_descriptor(service_id: u8, program_number: u16) -> [u8; 13] {
    let n = program_number.to_be_bytes();
    [0x25, 0x0B, 0x01, 0x00, 0xFF, KLVA[0], KLVA[1], KLVA[2], KLVA[3], service_id, 0x1F, n[0], n[1]]
}

/// Descriptor bytes (tag, length, data) as a [`Descriptor`].
fn descriptor(bytes: &[u8]) -> Descriptor {
    Descriptor { tag: bytes[0], data: bytes[2..].to_vec() }
}

/// The metadata_service_id of a metadata_descriptor body (Table 2-87).
pub(crate) fn metadata_service_id(body: &[u8]) -> Option<u8> {
    let app = u16::from_be_bytes([*body.first()?, *body.get(1)?]);
    let mut i = if app == 0xFFFF { 6 } else { 2 };
    if *body.get(i)? == 0xFF {
        i += 4;
    }
    body.get(i + 1).copied()
}

/// Options for a synchronous KLV stream. Start from
/// [`new`](Self::new) and change the fields you need:
///
/// ```
/// use mpegts::mux::{StreamSpec, SyncKlvOptions};
///
/// let mut options = SyncKlvOptions::new(1);
/// options.sync_registration_descriptor = true;
/// let spec = StreamSpec::klv_sync_with(&options)?;
/// assert_eq!(spec.descriptors.len(), 3);
/// # Ok::<(), mpegts::mux::MuxError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SyncKlvOptions {
    /// metadata_service_id, unique among all metadata services of the
    /// transport stream (R-ST1402TIME-35). Default 0.
    pub service_id: u8,
    /// Also write the "KLVA" registration_descriptor, for receivers that
    /// only recognise KLV by it (DECISIONS X-1). Default false.
    pub sync_registration_descriptor: bool,
    /// metadata_input_leak_rate, 22 bits, units of 400 bit/s. Default
    /// 0x3FFFFF (DECISIONS X-2).
    pub input_leak_rate: u32,
    /// metadata_buffer_size, 22 bits, units of 1024 bytes. Default 0x400
    /// (1 MiB, DECISIONS X-2).
    pub buffer_size: u32,
    /// metadata_output_leak_rate, 22 bits. Default 0 (R-ST1402TIME-31).
    pub output_leak_rate: u32,
    /// How cell sequence numbers are counted. Default
    /// [`CellNumbering::PerAccessUnit`] (DECISIONS X-8).
    pub cell_numbering: CellNumbering,
}

impl Default for SyncKlvOptions {
    fn default() -> Self {
        SyncKlvOptions {
            service_id: 0,
            sync_registration_descriptor: false,
            input_leak_rate: MAX_22_BITS,
            buffer_size: 0x400,
            output_leak_rate: 0,
            cell_numbering: CellNumbering::PerAccessUnit,
        }
    }
}

impl SyncKlvOptions {
    /// The defaults with this metadata_service_id.
    pub fn new(service_id: u8) -> Self {
        SyncKlvOptions { service_id, ..Self::default() }
    }
}

/// What a program needs to know about an elementary stream: its PMT entry
/// (stream_type and ES descriptors) and how its PES packets are written
/// (stream_id and [`Carriage`]). [`ProgramTables::add_stream`](crate::mux::ProgramTables::add_stream)
/// takes the first part, [`PesWriter::new`](crate::mux::PesWriter::new) the
/// second.
///
/// ```
/// use mpegts::mux::StreamSpec;
///
/// let spec = StreamSpec::klv_sync(0);
/// assert_eq!((spec.stream_type, spec.stream_id), (0x15, 0xFC));
/// assert_eq!(spec.descriptors.len(), 2); // metadata_descriptor, metadata_STD_descriptor
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StreamSpec {
    /// PMT stream_type (ISO/IEC 13818-1 Table 2-34).
    pub stream_type: u8,
    /// PES stream_id (Table 2-22).
    pub stream_id: u8,
    /// How PES packets are formed.
    pub carriage: Carriage,
    /// ES_info descriptors, in PMT order.
    pub descriptors: Vec<Descriptor>,
}

impl StreamSpec {
    /// A stream of plain PES packets with no descriptors.
    pub fn new(stream_type: u8, stream_id: u8) -> Self {
        StreamSpec { stream_type, stream_id, carriage: Carriage::Pes, descriptors: Vec::new() }
    }

    /// H.264 video: stream_type 0x1B, stream_id 0xE0.
    pub fn h264() -> Self {
        Self::new(0x1B, 0xE0)
    }

    /// H.265 video: stream_type 0x24, stream_id 0xE0.
    pub fn h265() -> Self {
        Self::new(0x24, 0xE0)
    }

    /// AAC audio with ADTS framing: stream_type 0x0F, stream_id 0xC0.
    pub fn aac_adts() -> Self {
        Self::new(0x0F, 0xC0)
    }

    /// Synchronous KLV (MISB ST 1402.2 synchronous metadata multiplex
    /// method) with the default [`SyncKlvOptions`]: stream_type 0x15 and
    /// stream_id 0xFC (R-ST1402TIME-16, R-ISO13818-099, R-ISO13818-061),
    /// ES descriptors `26 09 01 00 FF 4B 4C 56 41 <service> 0F` and
    /// `27 09 FF FF FF C0 04 00 C0 00 00` (DECISIONS section 2), PES
    /// packets of access unit cells with a PTS each.
    pub fn klv_sync(service_id: u8) -> Self {
        Self::klv_sync_with(&SyncKlvOptions::new(service_id)).expect("default options are valid")
    }

    /// Synchronous KLV with explicit options. Fails when a
    /// metadata_STD_descriptor field does not fit in 22 bits.
    pub fn klv_sync_with(options: &SyncKlvOptions) -> Result<Self, MuxError> {
        // One metadata_descriptor per service, then exactly one
        // metadata_STD_descriptor per stream, in the stream's ES_info loop
        // (R-ST1402TIME-25, R-ST1402TIME-26, st1402-time.md D-1). The
        // optional registration descriptor goes last (st1402-time.md 3.5).
        let std = metadata_std_descriptor(options.input_leak_rate, options.buffer_size, options.output_leak_rate)?;
        let mut descriptors = vec![descriptor(&klv_metadata_descriptor(options.service_id)), descriptor(&std)];
        if options.sync_registration_descriptor {
            descriptors.push(descriptor(&KLVA_REGISTRATION_DESCRIPTOR));
        }
        Ok(StreamSpec {
            stream_type: 0x15,
            stream_id: 0xFC,
            carriage: Carriage::MetadataCells { service_id: options.service_id },
            descriptors,
        })
    }

    /// Asynchronous KLV (SMPTE RP 217, the ST 1402.2 asynchronous method):
    /// stream_type 0x06 and stream_id 0xBD (R-ST1402TIME-40), the "KLVA"
    /// registration_descriptor (R-ST1402TIME-44), PES packets without PTS
    /// that each start with a KLV item (R-ST1402TIME-42, R-ST1402TIME-45).
    pub fn klv_async() -> Self {
        StreamSpec {
            stream_type: 0x06,
            stream_id: 0xBD,
            carriage: Carriage::AsyncKlv,
            descriptors: vec![descriptor(&KLVA_REGISTRATION_DESCRIPTOR)],
        }
    }

    /// Append an ES descriptor (tag and body, without the length byte).
    pub fn with_descriptor(mut self, tag: u8, data: &[u8]) -> Self {
        self.descriptors.push(Descriptor { tag, data: data.to_vec() });
        self
    }

    /// Carry another KLV service on this synchronous stream: adds its
    /// metadata_descriptor after the existing ones (R-ST1402TIME-23,
    /// R-ST1402TIME-25). Write its access units with
    /// [`PesUnit::with_service`](crate::mux::PesUnit::with_service).
    pub fn add_metadata_service(&mut self, service_id: u8) -> Result<(), MuxError> {
        if !matches!(self.carriage, Carriage::MetadataCells { .. }) {
            return Err(MuxError::InvalidValue {
                field: "carriage",
                reason: "metadata services are carried only in metadata access unit cells",
            });
        }
        if self.metadata_services().any(|s| s == service_id) {
            return Err(MuxError::InvalidValue {
                field: "metadata_service_id",
                reason: "already carried by this stream (R-ST1402TIME-35)",
            });
        }
        let at = self.descriptors.iter().rposition(|d| d.tag == 0x26).map_or(0, |i| i + 1);
        self.descriptors.insert(at, descriptor(&klv_metadata_descriptor(service_id)));
        Ok(())
    }

    /// The metadata_service_ids of the stream's metadata_descriptors.
    pub fn metadata_services(&self) -> impl Iterator<Item = u8> + '_ {
        self.descriptors.iter().filter(|d| d.tag == 0x26).filter_map(|d| metadata_service_id(&d.data))
    }

    /// The stream_id and carriage agree with each other.
    pub(crate) fn check_pes(stream_id: u8, carriage: Carriage) -> Result<(), MuxError> {
        let bad = |reason| Err(MuxError::InvalidValue { field: "stream_id", reason });
        // Values below 0xBC are start codes of other syntax, not stream_ids
        // (ISO/IEC 13818-1 Table 2-22).
        if stream_id < 0xBC {
            return bad("PES stream_id values start at 0xBC (Table 2-22)");
        }
        match carriage {
            Carriage::MetadataCells { .. } if stream_id != 0xFC => {
                bad("metadata access unit cells are carried with stream_id 0xFC (R-ISO13818-132, R-ST1402TIME-16)")
            }
            Carriage::AsyncKlv if stream_id != 0xBD => {
                bad("asynchronous KLV is carried with stream_id 0xBD (R-ST1402TIME-40)")
            }
            Carriage::Pes if stream_id == 0xFC => bad(
                "stream_id 0xFC carries metadata access unit cells; use Carriage::MetadataCells (R-ISO13818-132)",
            ),
            _ => Ok(()),
        }
    }

    /// Everything [`ProgramTables::add_stream`](crate::mux::ProgramTables::add_stream)
    /// relies on.
    pub(crate) fn check(&self) -> Result<(), MuxError> {
        Self::check_pes(self.stream_id, self.carriage)?;
        let bad = |field, reason| Err(MuxError::InvalidValue { field, reason });
        if self.descriptors.iter().any(|d| d.data.len() > 255) {
            return Err(MuxError::InvalidDescriptor("descriptor data longer than 255 bytes (R-ISO13818-105)"));
        }
        match self.carriage {
            Carriage::MetadataCells { service_id } => {
                if self.stream_type != 0x15 {
                    return bad("stream_type", "synchronous KLV is stream_type 0x15 (R-ST1402TIME-16)");
                }
                if !self.metadata_services().any(|s| s == service_id) {
                    return bad(
                        "descriptors",
                        "a metadata_descriptor for the carriage's service is required (R-ST1402TIME-25)",
                    );
                }
                if self.descriptors.iter().filter(|d| d.tag == 0x27).count() != 1 {
                    return bad("descriptors", "exactly one metadata_STD_descriptor is required (R-ST1402TIME-26)");
                }
            }
            Carriage::AsyncKlv => {
                if self.stream_type != 0x06 {
                    return bad("stream_type", "asynchronous KLV is stream_type 0x06 (R-ST1402TIME-40)");
                }
                if !self.descriptors.iter().any(|d| d.tag == 0x05 && d.data.starts_with(&KLVA)) {
                    return bad(
                        "descriptors",
                        "the \"KLVA\" registration_descriptor is required (R-ST1402TIME-44)",
                    );
                }
            }
            Carriage::Pes => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(d: &Descriptor) -> Vec<u8> {
        let mut v = vec![d.tag, d.data.len() as u8];
        v.extend_from_slice(&d.data);
        v
    }

    #[test]
    fn settled_descriptor_bytes() {
        let s = StreamSpec::klv_sync(0);
        let all: Vec<u8> = s.descriptors.iter().flat_map(bytes).collect();
        assert_eq!(
            all,
            [0x26, 0x09, 0x01, 0x00, 0xFF, 0x4B, 0x4C, 0x56, 0x41, 0x00, 0x0F, 0x27, 0x09, 0xFF, 0xFF, 0xFF, 0xC0, 0x04, 0x00, 0xC0, 0x00, 0x00]
        );
        assert_eq!(metadata_std_descriptor(0x3FFFFF, 0x400, 0).unwrap(), METADATA_STD_DESCRIPTOR);
        // The superseded phase 0 draft values, as a check of the bit layout.
        assert_eq!(
            metadata_std_descriptor(250_000, 128, 0).unwrap(),
            [0x27, 0x09, 0xC3, 0xD0, 0x90, 0xC0, 0x00, 0x80, 0xC0, 0x00, 0x00]
        );
        assert!(metadata_std_descriptor(1 << 22, 0, 0).is_err());
        assert_eq!(bytes(&StreamSpec::klv_async().descriptors[0]), [0x05, 0x04, 0x4B, 0x4C, 0x56, 0x41]);
        assert_eq!(
            klv_metadata_pointer_descriptor(0, 1),
            [0x25, 0x0B, 0x01, 0x00, 0xFF, 0x4B, 0x4C, 0x56, 0x41, 0x00, 0x1F, 0x00, 0x01]
        );
        let opts = SyncKlvOptions { sync_registration_descriptor: true, ..SyncKlvOptions::new(3) };
        let s = StreamSpec::klv_sync_with(&opts).unwrap();
        assert_eq!(s.descriptors.iter().map(|d| d.tag).collect::<Vec<_>>(), [0x26, 0x27, 0x05]);
        assert_eq!(s.metadata_services().collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn services_and_checks() {
        let mut s = StreamSpec::klv_sync(0);
        s.add_metadata_service(1).unwrap();
        assert!(s.add_metadata_service(1).is_err());
        assert_eq!(s.descriptors.iter().map(|d| d.tag).collect::<Vec<_>>(), [0x26, 0x26, 0x27]);
        assert_eq!(s.metadata_services().collect::<Vec<_>>(), [0, 1]);
        assert!(s.check().is_ok());
        assert!(StreamSpec::klv_async().add_metadata_service(0).is_err());
        assert_eq!(metadata_service_id(&[0xFF, 0xFF, 1, 2, 3, 4, 0xFF, 1, 2, 3, 4, 9]), Some(9));

        let mut wrong = StreamSpec::klv_sync(0);
        wrong.stream_type = 0x06;
        assert!(wrong.check().is_err());
        let mut wrong = StreamSpec::klv_async();
        wrong.descriptors.clear();
        assert!(wrong.check().is_err());
        assert!(StreamSpec::new(0x06, 0xFC).check().is_err());
        assert!(StreamSpec::new(0x06, 0xB9).check().is_err());
        assert!(StreamSpec::new(0x06, 0xBD).with_descriptor(0x80, &[0; 256]).check().is_err());
        assert!(StreamSpec::h264().check().is_ok());
    }
}
