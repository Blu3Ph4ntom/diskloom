use std::{
    io::{self, Read, Write},
    string::FromUtf8Error,
};

use diskloom_core::{EntryFlags, EntryId, EntryMetadata, FileGraph, FileGraphBuilder, FileKind};
use thiserror::Error;

const MAGIC: &[u8; 8] = b"DLSNP001";
const NO_PARENT: u32 = u32::MAX;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Utf8(#[from] FromUtf8Error),
    #[error(transparent)]
    Graph(#[from] diskloom_core::FileGraphError),
    #[error("invalid DiskLoom snapshot header")]
    InvalidHeader,
    #[error("snapshot entry {entry} has invalid parent {parent}")]
    InvalidParent { entry: u32, parent: u32 },
    #[error("snapshot contains too many entries")]
    TooManyEntries,
    #[error("snapshot string is too large")]
    StringTooLarge,
}

pub fn export_snapshot<W: Write>(graph: &FileGraph, mut writer: W) -> Result<(), SnapshotError> {
    let entry_count = u32::try_from(graph.len()).map_err(|_| SnapshotError::TooManyEntries)?;
    writer.write_all(MAGIC)?;
    write_u32(&mut writer, entry_count)?;

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        let Some(stats) = graph.stats(id) else {
            continue;
        };
        let name = graph.name(id).unwrap_or_default().as_bytes();
        let name_len = u32::try_from(name.len()).map_err(|_| SnapshotError::StringTooLarge)?;

        write_u32(
            &mut writer,
            entry.parent.map_or(NO_PARENT, |parent| parent.0),
        )?;
        write_u16(&mut writer, entry.flags.bits())?;
        write_i64(&mut writer, entry.modified_unix)?;
        write_u64(&mut writer, stats.own_size.bytes())?;
        write_u64(&mut writer, stats.own_allocated.bytes())?;
        write_u32(&mut writer, name_len)?;
        writer.write_all(name)?;
    }

    Ok(())
}

pub fn import_snapshot<R: Read>(mut reader: R) -> Result<FileGraph, SnapshotError> {
    let mut header = [0_u8; 8];
    reader.read_exact(&mut header)?;
    if &header != MAGIC {
        return Err(SnapshotError::InvalidHeader);
    }

    let entry_count = read_u32(&mut reader)?;
    let mut builder = FileGraphBuilder::without_name_dedup();
    for idx in 0..entry_count {
        let parent = read_u32(&mut reader)?;
        let flags = EntryFlags::from_bits_retain(read_u16(&mut reader)?);
        let modified_unix = read_i64(&mut reader)?;
        let size = read_u64(&mut reader)?;
        let allocated = read_u64(&mut reader)?;
        let name_len = read_u32(&mut reader)?;
        let name_len = usize::try_from(name_len).map_err(|_| SnapshotError::StringTooLarge)?;
        let mut name = vec![0_u8; name_len];
        reader.read_exact(&mut name)?;
        let name = String::from_utf8(name)?;

        let parent = if parent == NO_PARENT {
            None
        } else if parent < idx {
            Some(EntryId(parent))
        } else {
            return Err(SnapshotError::InvalidParent { entry: idx, parent });
        };
        let kind = kind_from_flags(flags);
        let mut extra_flags = flags;
        extra_flags.remove(EntryFlags::DIRECTORY | EntryFlags::SYMLINK);

        builder.add_entry_with_flags_owned_name(
            parent,
            name,
            EntryMetadata {
                kind,
                size,
                allocated,
                modified_unix,
                extra_flags,
            },
        )?;
    }

    Ok(builder.finish())
}

fn kind_from_flags(flags: EntryFlags) -> FileKind {
    if flags.contains(EntryFlags::DIRECTORY) {
        FileKind::Directory
    } else if flags.contains(EntryFlags::SYMLINK) {
        FileKind::Symlink
    } else {
        FileKind::File
    }
}

fn write_u16(writer: &mut impl Write, value: u16) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_i64(writer: &mut impl Write, value: i64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn read_u16(reader: &mut impl Read) -> io::Result<u16> {
    let mut bytes = [0_u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_i64(reader: &mut impl Read) -> io::Result<i64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(i64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use diskloom_core::{EntryFlags, EntryMetadata, FileGraphBuilder, FileKind};

    use super::{SnapshotError, export_snapshot, import_snapshot};

    #[test]
    fn snapshot_should_round_trip_graph_entries_and_totals() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "C:\\", FileKind::Directory, 0, 0, 10)
            .unwrap();
        let child = builder
            .add_entry_with_flags(
                Some(root),
                "file.bin",
                EntryMetadata {
                    kind: FileKind::File,
                    size: 10,
                    allocated: 16,
                    modified_unix: 20,
                    extra_flags: EntryFlags::HARD_LINK,
                },
            )
            .unwrap();
        let graph = builder.finish();

        let mut bytes = Vec::new();
        export_snapshot(&graph, &mut bytes).unwrap();
        let restored = import_snapshot(bytes.as_slice()).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.name(root), Some("C:\\"));
        assert_eq!(restored.name(child), Some("file.bin"));
        assert_eq!(restored.stats(root).unwrap().total_size.bytes(), 10);
        assert!(
            restored
                .entry(child)
                .unwrap()
                .flags
                .contains(EntryFlags::HARD_LINK)
        );
    }

    #[test]
    fn snapshot_should_reject_invalid_header() {
        let error = import_snapshot(b"bad-data".as_slice()).unwrap_err();

        assert!(matches!(error, SnapshotError::InvalidHeader));
    }
}
