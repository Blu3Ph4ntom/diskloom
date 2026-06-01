use thiserror::Error;

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
    use super::{FileRecordHeader, MftParseError};

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
}
