use thiserror::Error;

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_END: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRecordHeader {
    pub sequence_number: u16,
    pub hard_link_count: u16,
    pub first_attribute_offset: u16,
    pub flags: u16,
    pub bytes_in_use: u32,
    pub bytes_allocated: u32,
    pub base_file_record: u64,
    pub next_attribute_id: u16,
    pub record_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFileRecord {
    pub header: FileRecordHeader,
    pub file_names: Vec<FileNameAttribute>,
    pub data_size: u64,
    pub allocated_size: u64,
    pub data_runs: Vec<DataRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNameAttribute {
    pub parent_reference: u64,
    pub parent_record_number: u64,
    pub allocated_size: u64,
    pub data_size: u64,
    pub flags: u32,
    pub modified_unix: i64,
    pub namespace: u8,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRun {
    pub lcn: Option<i64>,
    pub clusters: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttributeHeader {
    kind: u32,
    length: u32,
    non_resident: bool,
}

impl FileRecordHeader {
    pub fn parse(record: &[u8]) -> Result<Self, MftParseError> {
        if record.len() < 48 {
            return Err(MftParseError::TooShort {
                actual: record.len(),
            });
        }

        if &record[0..4] != b"FILE" {
            return Err(MftParseError::InvalidSignature);
        }

        Ok(Self {
            sequence_number: read_u16(record, 16),
            hard_link_count: read_u16(record, 18),
            first_attribute_offset: read_u16(record, 20),
            flags: read_u16(record, 22),
            bytes_in_use: read_u32(record, 24),
            bytes_allocated: read_u32(record, 28),
            base_file_record: read_u64(record, 32),
            next_attribute_id: read_u16(record, 40),
            record_number: read_u32(record, 44),
        })
    }

    #[must_use]
    pub const fn is_directory(self) -> bool {
        self.flags & 0x0002 != 0
    }

    #[must_use]
    pub const fn is_in_use(self) -> bool {
        self.flags & 0x0001 != 0
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MftParseError {
    #[error("MFT file record is too short: {actual} bytes")]
    TooShort { actual: usize },
    #[error("MFT file record does not start with FILE signature")]
    InvalidSignature,
    #[error("MFT update sequence array is invalid")]
    InvalidFixup,
    #[error("MFT attribute at offset {offset} has invalid length {length}")]
    InvalidAttributeLength { offset: usize, length: u32 },
    #[error("NTFS runlist is invalid")]
    InvalidRunlist,
    #[error("NTFS file name attribute is invalid")]
    InvalidFileName,
}

pub fn parse_file_record(
    record: &[u8],
    bytes_per_sector: u32,
) -> Result<ParsedFileRecord, MftParseError> {
    let mut fixed = record.to_vec();
    apply_fixups(&mut fixed, bytes_per_sector as usize)?;
    let header = FileRecordHeader::parse(&fixed)?;
    let mut parsed = ParsedFileRecord {
        header,
        file_names: Vec::new(),
        data_size: 0,
        allocated_size: 0,
        data_runs: Vec::new(),
    };

    for attribute in iter_attributes(&fixed, header)? {
        match attribute.header.kind {
            ATTR_FILE_NAME if !attribute.header.non_resident => {
                parsed.file_names.push(parse_file_name(attribute.body)?);
            }
            ATTR_DATA => {
                parse_data_attribute(attribute, &mut parsed)?;
            }
            _ => {}
        }
    }

    Ok(parsed)
}

pub fn apply_fixups(record: &mut [u8], bytes_per_sector: usize) -> Result<(), MftParseError> {
    if record.len() < 8 || bytes_per_sector < 2 {
        return Err(MftParseError::InvalidFixup);
    }

    let usa_offset = read_u16(record, 4) as usize;
    let usa_count = read_u16(record, 6) as usize;
    if usa_count == 0 || usa_offset + usa_count * 2 > record.len() {
        return Err(MftParseError::InvalidFixup);
    }

    let sectors = usa_count - 1;
    if sectors * bytes_per_sector > record.len() {
        return Err(MftParseError::InvalidFixup);
    }

    let update_sequence = [record[usa_offset], record[usa_offset + 1]];
    for sector_idx in 0..sectors {
        let fixup_position = (sector_idx + 1) * bytes_per_sector - 2;
        if record[fixup_position..fixup_position + 2] != update_sequence {
            return Err(MftParseError::InvalidFixup);
        }

        let replacement = usa_offset + (sector_idx + 1) * 2;
        record[fixup_position] = record[replacement];
        record[fixup_position + 1] = record[replacement + 1];
    }

    Ok(())
}

pub fn parse_runlist(input: &[u8]) -> Result<Vec<DataRun>, MftParseError> {
    let mut runs = Vec::new();
    let mut offset = 0;
    let mut current_lcn = 0_i64;

    while offset < input.len() {
        let header = input[offset];
        offset += 1;
        if header == 0 {
            return Ok(runs);
        }

        let length_size = (header & 0x0F) as usize;
        let offset_size = (header >> 4) as usize;
        if length_size == 0 || length_size > 8 || offset_size > 8 {
            return Err(MftParseError::InvalidRunlist);
        }
        if offset + length_size + offset_size > input.len() {
            return Err(MftParseError::InvalidRunlist);
        }

        let clusters = read_uint_le(&input[offset..offset + length_size]);
        offset += length_size;

        let lcn = if offset_size == 0 {
            None
        } else {
            let delta = read_int_le(&input[offset..offset + offset_size]);
            offset += offset_size;
            current_lcn = current_lcn.saturating_add(delta);
            Some(current_lcn)
        };

        runs.push(DataRun { lcn, clusters });
    }

    Err(MftParseError::InvalidRunlist)
}

#[derive(Debug, Clone, Copy)]
struct Attribute<'a> {
    header: AttributeHeader,
    body: &'a [u8],
}

fn iter_attributes(
    record: &[u8],
    header: FileRecordHeader,
) -> Result<Vec<Attribute<'_>>, MftParseError> {
    let mut attributes = Vec::new();
    let mut offset = header.first_attribute_offset as usize;
    let end = usize::min(header.bytes_in_use as usize, record.len());

    while offset + 8 <= end {
        let kind = read_u32(record, offset);
        if kind == ATTR_END {
            return Ok(attributes);
        }

        let length = read_u32(record, offset + 4);
        if length < 16 || offset + length as usize > end {
            return Err(MftParseError::InvalidAttributeLength { offset, length });
        }

        let non_resident = record[offset + 8] != 0;
        attributes.push(Attribute {
            header: AttributeHeader {
                kind,
                length,
                non_resident,
            },
            body: &record[offset..offset + length as usize],
        });
        offset += length as usize;
    }

    Ok(attributes)
}

fn parse_data_attribute(
    attribute: Attribute<'_>,
    parsed: &mut ParsedFileRecord,
) -> Result<(), MftParseError> {
    if attribute.header.non_resident {
        if attribute.body.len() < 64 {
            return Err(MftParseError::InvalidAttributeLength {
                offset: 0,
                length: attribute.header.length,
            });
        }

        let runlist_offset = read_u16(attribute.body, 32) as usize;
        if runlist_offset >= attribute.body.len() {
            return Err(MftParseError::InvalidRunlist);
        }

        parsed.allocated_size = read_u64(attribute.body, 40);
        parsed.data_size = read_u64(attribute.body, 48);
        parsed.data_runs = parse_runlist(&attribute.body[runlist_offset..])?;
    } else {
        if attribute.body.len() < 24 {
            return Err(MftParseError::InvalidAttributeLength {
                offset: 0,
                length: attribute.header.length,
            });
        }
        parsed.data_size = read_u32(attribute.body, 16) as u64;
        parsed.allocated_size = parsed.data_size;
    }

    Ok(())
}

fn parse_file_name(input: &[u8]) -> Result<FileNameAttribute, MftParseError> {
    if input.len() < 66 {
        return Err(MftParseError::InvalidFileName);
    }

    let value = resident_value(input)?;
    if value.len() < 66 {
        return Err(MftParseError::InvalidFileName);
    }

    let name_len = value[64] as usize;
    let name_bytes = name_len
        .checked_mul(2)
        .and_then(|bytes| 66_usize.checked_add(bytes))
        .ok_or(MftParseError::InvalidFileName)?;
    if name_bytes > value.len() {
        return Err(MftParseError::InvalidFileName);
    }

    let name_units = value[66..name_bytes]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let parent_reference = read_u64(value, 0);

    Ok(FileNameAttribute {
        parent_reference,
        parent_record_number: parent_reference & 0x0000_FFFF_FFFF_FFFF,
        allocated_size: read_u64(value, 40),
        data_size: read_u64(value, 48),
        flags: read_u32(value, 56),
        modified_unix: filetime_to_unix(read_u64(value, 16)),
        namespace: value[65],
        name: String::from_utf16_lossy(&name_units),
    })
}

fn resident_value(attribute: &[u8]) -> Result<&[u8], MftParseError> {
    if attribute.len() < 24 {
        return Err(MftParseError::InvalidAttributeLength {
            offset: 0,
            length: attribute.len() as u32,
        });
    }

    let value_length = read_u32(attribute, 16) as usize;
    let value_offset = read_u16(attribute, 20) as usize;
    if value_offset + value_length > attribute.len() {
        return Err(MftParseError::InvalidAttributeLength {
            offset: value_offset,
            length: value_length as u32,
        });
    }

    Ok(&attribute[value_offset..value_offset + value_length])
}

fn filetime_to_unix(filetime: u64) -> i64 {
    const WINDOWS_TO_UNIX_SECONDS: i64 = 11_644_473_600;
    let seconds = (filetime / 10_000_000) as i64;
    seconds.saturating_sub(WINDOWS_TO_UNIX_SECONDS)
}

fn read_uint_le(input: &[u8]) -> u64 {
    input.iter().enumerate().fold(0_u64, |value, (idx, byte)| {
        value | ((*byte as u64) << (idx * 8))
    })
}

fn read_int_le(input: &[u8]) -> i64 {
    let unsigned = read_uint_le(input);
    let bits = input.len() * 8;
    if bits == 64 {
        return unsigned as i64;
    }

    let sign_bit = 1_u64 << (bits - 1);
    if unsigned & sign_bit == 0 {
        unsigned as i64
    } else {
        (unsigned | (!0_u64 << bits)) as i64
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        ATTR_DATA, ATTR_END, ATTR_FILE_NAME, DataRun, FileRecordHeader, MftParseError,
        apply_fixups, parse_file_record, parse_runlist,
    };

    #[test]
    fn parse_should_read_file_record_header() {
        let mut record = [0_u8; 64];
        record[0..4].copy_from_slice(b"FILE");
        record[16..18].copy_from_slice(&7_u16.to_le_bytes());
        record[18..20].copy_from_slice(&2_u16.to_le_bytes());
        record[20..22].copy_from_slice(&56_u16.to_le_bytes());
        record[22..24].copy_from_slice(&3_u16.to_le_bytes());
        record[24..28].copy_from_slice(&256_u32.to_le_bytes());
        record[28..32].copy_from_slice(&1024_u32.to_le_bytes());
        record[32..40].copy_from_slice(&9_u64.to_le_bytes());
        record[40..42].copy_from_slice(&4_u16.to_le_bytes());
        record[44..48].copy_from_slice(&42_u32.to_le_bytes());

        let header = FileRecordHeader::parse(&record).unwrap();

        assert_eq!(header.sequence_number, 7);
        assert_eq!(header.hard_link_count, 2);
        assert!(header.is_in_use());
        assert!(header.is_directory());
    }

    #[test]
    fn parse_should_reject_invalid_signature() {
        let record = [0_u8; 64];

        let error = FileRecordHeader::parse(&record).unwrap_err();

        assert_eq!(error, MftParseError::InvalidSignature);
    }

    #[test]
    fn apply_fixups_should_restore_sector_trailers() {
        let mut record = [0_u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&48_u16.to_le_bytes());
        record[6..8].copy_from_slice(&3_u16.to_le_bytes());
        record[48..50].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[50..52].copy_from_slice(&0xBBBB_u16.to_le_bytes());
        record[52..54].copy_from_slice(&0xCCCC_u16.to_le_bytes());
        record[510..512].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[1022..1024].copy_from_slice(&0xAAAA_u16.to_le_bytes());

        apply_fixups(&mut record, 512).unwrap();

        assert_eq!(&record[510..512], &0xBBBB_u16.to_le_bytes());
        assert_eq!(&record[1022..1024], &0xCCCC_u16.to_le_bytes());
    }

    #[test]
    fn parse_runlist_should_decode_signed_relative_offsets() {
        let runs = parse_runlist(&[
            0x11, 0x03, 0x20, // 3 clusters at LCN 32
            0x11, 0x02, 0xF0, // 2 clusters at LCN 16
            0x00,
        ])
        .unwrap();

        assert_eq!(
            runs,
            [
                DataRun {
                    lcn: Some(32),
                    clusters: 3
                },
                DataRun {
                    lcn: Some(16),
                    clusters: 2
                }
            ]
        );
    }

    #[test]
    fn parse_file_record_should_read_file_name_and_data_runs() {
        let mut record = [0_u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&48_u16.to_le_bytes());
        record[6..8].copy_from_slice(&3_u16.to_le_bytes());
        record[16..18].copy_from_slice(&1_u16.to_le_bytes());
        record[18..20].copy_from_slice(&1_u16.to_le_bytes());
        record[20..22].copy_from_slice(&56_u16.to_le_bytes());
        record[22..24].copy_from_slice(&1_u16.to_le_bytes());
        record[24..28].copy_from_slice(&256_u32.to_le_bytes());
        record[28..32].copy_from_slice(&1024_u32.to_le_bytes());
        record[44..48].copy_from_slice(&42_u32.to_le_bytes());
        record[48..50].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[50..52].copy_from_slice(&0_u16.to_le_bytes());
        record[52..54].copy_from_slice(&0_u16.to_le_bytes());
        record[510..512].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[1022..1024].copy_from_slice(&0xAAAA_u16.to_le_bytes());

        let name_offset = 56_usize;
        let name_attr_len = write_file_name_attribute(&mut record[name_offset..], "hello.txt");
        let data_offset = name_offset + name_attr_len;
        let data_attr_len = write_data_attribute(&mut record[data_offset..]);
        let end = data_offset + data_attr_len;
        record[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        record[24..28].copy_from_slice(&(end as u32 + 4).to_le_bytes());

        let parsed = parse_file_record(&record, 512).unwrap();

        assert_eq!(parsed.header.record_number, 42);
        assert_eq!(parsed.file_names[0].name, "hello.txt");
        assert_eq!(parsed.data_size, 16);
        assert_eq!(
            parsed.data_runs,
            [DataRun {
                lcn: Some(32),
                clusters: 4
            }]
        );
    }

    fn write_file_name_attribute(output: &mut [u8], name: &str) -> usize {
        let name_units = name.encode_utf16().collect::<Vec<_>>();
        let value_len = 66 + name_units.len() * 2;
        let attr_len = 24 + value_len;
        output[0..4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        output[4..8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        output[16..20].copy_from_slice(&(value_len as u32).to_le_bytes());
        output[20..22].copy_from_slice(&24_u16.to_le_bytes());
        output[24..32].copy_from_slice(&5_u64.to_le_bytes());
        output[64..72].copy_from_slice(&123_u64.to_le_bytes());
        output[72..80].copy_from_slice(&456_u64.to_le_bytes());
        output[80..84].copy_from_slice(&0_u32.to_le_bytes());
        output[88] = name_units.len() as u8;
        output[89] = 1;
        for (idx, unit) in name_units.iter().enumerate() {
            let start = 90 + idx * 2;
            output[start..start + 2].copy_from_slice(&unit.to_le_bytes());
        }
        attr_len
    }

    fn write_data_attribute(output: &mut [u8]) -> usize {
        let runlist = [0x11, 0x04, 0x20, 0x00];
        let attr_len = 64 + runlist.len();
        output[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        output[4..8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        output[8] = 1;
        output[32..34].copy_from_slice(&64_u16.to_le_bytes());
        output[40..48].copy_from_slice(&16_u64.to_le_bytes());
        output[48..56].copy_from_slice(&16_u64.to_le_bytes());
        output[64..64 + runlist.len()].copy_from_slice(&runlist);
        attr_len
    }
}
