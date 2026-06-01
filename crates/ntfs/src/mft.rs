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
    pub standard_information: Option<StandardInformationAttribute>,
    pub file_names: Vec<FileNameAttribute>,
    pub data_size: u64,
    pub allocated_size: u64,
    pub data_runs: Vec<DataRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFileRecord {
    pub header: FileRecordHeader,
    pub standard_modified_unix: Option<i64>,
    pub file_name: Option<ScannedFileNameAttribute>,
    pub data_size: u64,
    pub allocated_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardInformationAttribute {
    pub created_unix: i64,
    pub modified_unix: i64,
    pub mft_changed_unix: i64,
    pub accessed_unix: i64,
    pub file_attributes: u32,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFileNameAttribute {
    pub parent_record_number: u64,
    pub allocated_size: u64,
    pub data_size: u64,
    pub modified_unix: i64,
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

impl ScannedFileRecord {
    fn with_header(header: FileRecordHeader) -> Self {
        Self {
            header,
            standard_modified_unix: None,
            file_name: None,
            data_size: 0,
            allocated_size: 0,
        }
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
    #[error("NTFS standard information attribute is invalid")]
    InvalidStandardInformation,
    #[error("NTFS file name attribute is invalid")]
    InvalidFileName,
}

pub fn parse_file_record(
    record: &[u8],
    bytes_per_sector: u32,
) -> Result<ParsedFileRecord, MftParseError> {
    let mut fixed = record.to_vec();
    apply_fixups(&mut fixed, bytes_per_sector as usize)?;
    parse_fixed_file_record(&fixed)
}

pub fn parse_file_record_reuse(
    record: &[u8],
    bytes_per_sector: u32,
    scratch: &mut Vec<u8>,
) -> Result<ParsedFileRecord, MftParseError> {
    scratch.clear();
    scratch.extend_from_slice(record);
    apply_fixups(scratch, bytes_per_sector as usize)?;
    parse_fixed_file_record(scratch)
}

pub fn parse_scanned_file_record(
    record: &[u8],
    bytes_per_sector: u32,
) -> Result<ScannedFileRecord, MftParseError> {
    let header = FileRecordHeader::parse(record)?;
    if !header.is_in_use() || header.base_file_record != 0 {
        return Ok(ScannedFileRecord::with_header(header));
    }

    let mut fixed = record.to_vec();
    apply_fixups(&mut fixed, bytes_per_sector as usize)?;
    parse_fixed_scanned_file_record_with_header(&fixed, header)
}

pub fn parse_scanned_file_record_reuse(
    record: &[u8],
    bytes_per_sector: u32,
    scratch: &mut Vec<u8>,
) -> Result<ScannedFileRecord, MftParseError> {
    let header = FileRecordHeader::parse(record)?;
    if !header.is_in_use() || header.base_file_record != 0 {
        return Ok(ScannedFileRecord::with_header(header));
    }

    scratch.clear();
    scratch.extend_from_slice(record);
    apply_fixups(scratch, bytes_per_sector as usize)?;
    parse_fixed_scanned_file_record_with_header(scratch, header)
}

pub fn parse_scanned_file_record_in_place(
    record: &mut [u8],
    bytes_per_sector: u32,
) -> Result<ScannedFileRecord, MftParseError> {
    let header = FileRecordHeader::parse(record)?;
    if !header.is_in_use() || header.base_file_record != 0 {
        return Ok(ScannedFileRecord::with_header(header));
    }

    apply_fixups(record, bytes_per_sector as usize)?;
    parse_fixed_scanned_file_record_with_header(record, header)
}

fn parse_fixed_file_record(record: &[u8]) -> Result<ParsedFileRecord, MftParseError> {
    let header = FileRecordHeader::parse(record)?;
    let mut parsed = ParsedFileRecord {
        header,
        standard_information: None,
        file_names: Vec::new(),
        data_size: 0,
        allocated_size: 0,
        data_runs: Vec::new(),
    };

    let mut offset = header.first_attribute_offset as usize;
    let end = usize::min(header.bytes_in_use as usize, record.len());

    while offset + 8 <= end {
        let kind = read_u32(record, offset);
        if kind == ATTR_END {
            break;
        }

        let length = read_u32(record, offset + 4);
        if length < 16 || offset + length as usize > end {
            return Err(MftParseError::InvalidAttributeLength { offset, length });
        }

        let non_resident = record[offset + 8] != 0;
        let attribute = Attribute {
            header: AttributeHeader {
                kind,
                length,
                non_resident,
            },
            body: &record[offset..offset + length as usize],
        };
        match attribute.header.kind {
            ATTR_STANDARD_INFORMATION if !attribute.header.non_resident => {
                parsed.standard_information = Some(parse_standard_information(attribute.body)?);
            }
            ATTR_FILE_NAME if !attribute.header.non_resident => {
                parsed.file_names.push(parse_file_name(attribute.body)?);
            }
            ATTR_DATA => {
                parse_data_attribute(attribute, &mut parsed)?;
            }
            _ => {}
        }

        offset += length as usize;
    }

    Ok(parsed)
}

fn parse_fixed_scanned_file_record_with_header(
    record: &[u8],
    header: FileRecordHeader,
) -> Result<ScannedFileRecord, MftParseError> {
    let mut parsed = ScannedFileRecord::with_header(header);
    let mut best_name_priority = None;
    let mut best_name_value = None;
    let is_directory = header.is_directory();

    let mut offset = header.first_attribute_offset as usize;
    let end = usize::min(header.bytes_in_use as usize, record.len());

    while offset + 8 <= end {
        let kind = read_u32(record, offset);
        if kind == ATTR_END {
            break;
        }

        let length = read_u32(record, offset + 4);
        if length < 16 || offset + length as usize > end {
            return Err(MftParseError::InvalidAttributeLength { offset, length });
        }

        let non_resident = record[offset + 8] != 0;
        let attribute = Attribute {
            header: AttributeHeader {
                kind,
                length,
                non_resident,
            },
            body: &record[offset..offset + length as usize],
        };
        match attribute.header.kind {
            ATTR_STANDARD_INFORMATION if !attribute.header.non_resident => {
                parsed.standard_modified_unix =
                    Some(parse_standard_information_modified_unix(attribute.body)?);
            }
            ATTR_FILE_NAME if !attribute.header.non_resident => {
                let value = file_name_value(attribute.body)?;
                let priority = file_name_namespace_priority(value[65]);
                if !matches!(best_name_priority, Some(current) if priority <= current) {
                    best_name_value = Some(value);
                    best_name_priority = Some(priority);
                }
            }
            ATTR_DATA if !is_directory => {
                parse_scanned_data_attribute(attribute, &mut parsed)?;
            }
            _ => {}
        }

        offset += length as usize;
    }

    if let Some(value) = best_name_value {
        parsed.file_name = Some(decode_scanned_file_name(
            value,
            parsed.standard_modified_unix.is_none(),
        )?);
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

fn parse_scanned_data_attribute(
    attribute: Attribute<'_>,
    parsed: &mut ScannedFileRecord,
) -> Result<(), MftParseError> {
    if attribute.header.non_resident {
        if attribute.body.len() < 64 {
            return Err(MftParseError::InvalidAttributeLength {
                offset: 0,
                length: attribute.header.length,
            });
        }

        parsed.allocated_size = read_u64(attribute.body, 40);
        parsed.data_size = read_u64(attribute.body, 48);
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

fn parse_standard_information(input: &[u8]) -> Result<StandardInformationAttribute, MftParseError> {
    let value = resident_value(input)?;
    if value.len() < 32 {
        return Err(MftParseError::InvalidStandardInformation);
    }

    Ok(StandardInformationAttribute {
        created_unix: filetime_to_unix(read_u64(value, 0)),
        modified_unix: filetime_to_unix(read_u64(value, 8)),
        mft_changed_unix: filetime_to_unix(read_u64(value, 16)),
        accessed_unix: filetime_to_unix(read_u64(value, 24)),
        file_attributes: if value.len() >= 36 {
            read_u32(value, 32)
        } else {
            0
        },
    })
}

fn parse_standard_information_modified_unix(input: &[u8]) -> Result<i64, MftParseError> {
    let value = resident_value(input)?;
    if value.len() < 16 {
        return Err(MftParseError::InvalidStandardInformation);
    }

    Ok(filetime_to_unix(read_u64(value, 8)))
}

fn parse_file_name(input: &[u8]) -> Result<FileNameAttribute, MftParseError> {
    decode_file_name(file_name_value(input)?)
}

fn file_name_value(input: &[u8]) -> Result<&[u8], MftParseError> {
    if input.len() < 66 {
        return Err(MftParseError::InvalidFileName);
    }

    let value = resident_value(input)?;
    if value.len() < 66 {
        return Err(MftParseError::InvalidFileName);
    }

    Ok(value)
}

fn decode_file_name(value: &[u8]) -> Result<FileNameAttribute, MftParseError> {
    let name_len = value[64] as usize;
    let name_bytes = name_len
        .checked_mul(2)
        .and_then(|bytes| 66_usize.checked_add(bytes))
        .ok_or(MftParseError::InvalidFileName)?;
    if name_bytes > value.len() {
        return Err(MftParseError::InvalidFileName);
    }

    let name = decode_file_name_string(&value[66..name_bytes], name_len);
    let parent_reference = read_u64(value, 0);

    Ok(FileNameAttribute {
        parent_reference,
        parent_record_number: parent_reference & 0x0000_FFFF_FFFF_FFFF,
        allocated_size: read_u64(value, 40),
        data_size: read_u64(value, 48),
        flags: read_u32(value, 56),
        modified_unix: filetime_to_unix(read_u64(value, 16)),
        namespace: value[65],
        name,
    })
}

fn decode_scanned_file_name(
    value: &[u8],
    include_modified_time: bool,
) -> Result<ScannedFileNameAttribute, MftParseError> {
    let name_len = value[64] as usize;
    let name_bytes = name_len
        .checked_mul(2)
        .and_then(|bytes| 66_usize.checked_add(bytes))
        .ok_or(MftParseError::InvalidFileName)?;
    if name_bytes > value.len() {
        return Err(MftParseError::InvalidFileName);
    }

    let name = decode_file_name_string(&value[66..name_bytes], name_len);
    let parent_reference = read_u64(value, 0);

    Ok(ScannedFileNameAttribute {
        parent_record_number: parent_reference & 0x0000_FFFF_FFFF_FFFF,
        allocated_size: read_u64(value, 40),
        data_size: read_u64(value, 48),
        modified_unix: if include_modified_time {
            filetime_to_unix(read_u64(value, 16))
        } else {
            0
        },
        name,
    })
}

fn decode_file_name_string(input: &[u8], name_len: usize) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(name_len);
    let output = bytes.as_mut_ptr();
    for (idx, chunk) in input.chunks_exact(2).enumerate() {
        if chunk[1] != 0 || !chunk[0].is_ascii() {
            return decode_file_name_string_fallback(input, name_len);
        }
        // SAFETY: `bytes` has capacity `name_len`, and the input was validated to contain
        // exactly `name_len` UTF-16 code units before this function was called.
        unsafe {
            output.add(idx).write(chunk[0]);
        }
    }

    // SAFETY: The branch above accepts only single-byte ASCII, which is valid UTF-8.
    unsafe {
        bytes.set_len(name_len);
        String::from_utf8_unchecked(bytes)
    }
}

fn decode_file_name_string_fallback(input: &[u8], name_len: usize) -> String {
    let name_units = input
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    let mut name = String::with_capacity(name_len);
    for ch in char::decode_utf16(name_units) {
        name.push(ch.unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    name
}

fn file_name_namespace_priority(namespace: u8) -> u8 {
    match namespace {
        1 | 3 => 3,
        0 => 2,
        2 => 1,
        _ => 0,
    }
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
        ATTR_DATA, ATTR_END, ATTR_FILE_NAME, ATTR_STANDARD_INFORMATION, DataRun, FileRecordHeader,
        MftParseError, apply_fixups, parse_file_record, parse_runlist, parse_scanned_file_record,
        parse_scanned_file_record_in_place,
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

        let standard_offset = 56_usize;
        let standard_attr_len =
            write_standard_information_attribute(&mut record[standard_offset..]);
        let name_offset = standard_offset + standard_attr_len;
        let name_attr_len = write_file_name_attribute(&mut record[name_offset..], "hello.txt");
        let data_offset = name_offset + name_attr_len;
        let data_attr_len = write_data_attribute(&mut record[data_offset..]);
        let end = data_offset + data_attr_len;
        record[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        record[24..28].copy_from_slice(&(end as u32 + 4).to_le_bytes());

        let parsed = parse_file_record(&record, 512).unwrap();

        assert_eq!(parsed.header.record_number, 42);
        let standard_information = parsed.standard_information.unwrap();
        assert_eq!(standard_information.modified_unix, 200);
        assert_eq!(standard_information.file_attributes, 0x20);
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

    #[test]
    fn parse_scanned_file_record_should_skip_data_run_decoding() {
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
        let data_attr_len = write_invalid_runlist_data_attribute(&mut record[data_offset..]);
        let end = data_offset + data_attr_len;
        record[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        record[24..28].copy_from_slice(&(end as u32 + 4).to_le_bytes());

        let parsed = parse_scanned_file_record(&record, 512).unwrap();

        assert_eq!(parsed.file_name.unwrap().name, "hello.txt");
        assert_eq!(parsed.data_size, 16);
        assert_eq!(parsed.allocated_size, 32);
        assert!(parse_file_record(&record, 512).is_err());
    }

    #[test]
    fn parse_scanned_file_record_in_place_should_apply_fixups() {
        let mut record = [0_u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&48_u16.to_le_bytes());
        record[6..8].copy_from_slice(&3_u16.to_le_bytes());
        record[16..18].copy_from_slice(&1_u16.to_le_bytes());
        record[18..20].copy_from_slice(&1_u16.to_le_bytes());
        record[20..22].copy_from_slice(&56_u16.to_le_bytes());
        record[22..24].copy_from_slice(&1_u16.to_le_bytes());
        record[24..28].copy_from_slice(&64_u32.to_le_bytes());
        record[28..32].copy_from_slice(&1024_u32.to_le_bytes());
        record[44..48].copy_from_slice(&42_u32.to_le_bytes());
        record[48..50].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[50..52].copy_from_slice(&0xBBBB_u16.to_le_bytes());
        record[52..54].copy_from_slice(&0xCCCC_u16.to_le_bytes());
        record[56..60].copy_from_slice(&ATTR_END.to_le_bytes());
        record[510..512].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[1022..1024].copy_from_slice(&0xAAAA_u16.to_le_bytes());

        parse_scanned_file_record_in_place(&mut record, 512).unwrap();

        assert_eq!(&record[510..512], &0xBBBB_u16.to_le_bytes());
        assert_eq!(&record[1022..1024], &0xCCCC_u16.to_le_bytes());
    }

    #[test]
    fn parse_file_record_should_decode_ascii_file_name() {
        let mut record = [0_u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&48_u16.to_le_bytes());
        record[6..8].copy_from_slice(&3_u16.to_le_bytes());
        record[16..18].copy_from_slice(&1_u16.to_le_bytes());
        record[18..20].copy_from_slice(&1_u16.to_le_bytes());
        record[20..22].copy_from_slice(&56_u16.to_le_bytes());
        record[22..24].copy_from_slice(&1_u16.to_le_bytes());
        record[28..32].copy_from_slice(&1024_u32.to_le_bytes());
        record[48..50].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[50..52].copy_from_slice(&0_u16.to_le_bytes());
        record[52..54].copy_from_slice(&0_u16.to_le_bytes());
        record[510..512].copy_from_slice(&0xAAAA_u16.to_le_bytes());
        record[1022..1024].copy_from_slice(&0xAAAA_u16.to_le_bytes());

        let name_attr_len = write_file_name_attribute(&mut record[56..], "readme.md");
        let end = 56 + name_attr_len;
        record[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        record[24..28].copy_from_slice(&(end as u32 + 4).to_le_bytes());

        let parsed = parse_scanned_file_record(&record, 512).unwrap();

        assert_eq!(parsed.file_name.unwrap().name, "readme.md");
    }

    fn write_standard_information_attribute(output: &mut [u8]) -> usize {
        let value_len = 36_usize;
        let attr_len = 24 + value_len;
        output[0..4].copy_from_slice(&ATTR_STANDARD_INFORMATION.to_le_bytes());
        output[4..8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        output[16..20].copy_from_slice(&(value_len as u32).to_le_bytes());
        output[20..22].copy_from_slice(&24_u16.to_le_bytes());
        output[24..32].copy_from_slice(&unix_to_filetime(100).to_le_bytes());
        output[32..40].copy_from_slice(&unix_to_filetime(200).to_le_bytes());
        output[40..48].copy_from_slice(&unix_to_filetime(300).to_le_bytes());
        output[48..56].copy_from_slice(&unix_to_filetime(400).to_le_bytes());
        output[56..60].copy_from_slice(&0x20_u32.to_le_bytes());
        attr_len
    }

    fn unix_to_filetime(seconds: i64) -> u64 {
        const WINDOWS_TO_UNIX_SECONDS: i64 = 11_644_473_600;
        ((seconds + WINDOWS_TO_UNIX_SECONDS) as u64) * 10_000_000
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

    fn write_invalid_runlist_data_attribute(output: &mut [u8]) -> usize {
        let runlist = [0xFF, 0x00];
        let attr_len = 64 + runlist.len();
        output[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        output[4..8].copy_from_slice(&(attr_len as u32).to_le_bytes());
        output[8] = 1;
        output[32..34].copy_from_slice(&64_u16.to_le_bytes());
        output[40..48].copy_from_slice(&32_u64.to_le_bytes());
        output[48..56].copy_from_slice(&16_u64.to_le_bytes());
        output[64..64 + runlist.len()].copy_from_slice(&runlist);
        attr_len
    }
}
