use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use diskloom_core::{
    EntryFlags, EntryId, EntryMetadata, FileGraph, FileGraphBuilder, FileGraphError, FileKind,
};
use thiserror::Error;

use crate::mft::{FileNameAttribute, MftParseError, ParsedFileRecord, parse_file_record};

const ROOT_RECORD_NUMBER: u64 = 5;

#[derive(Debug, Default)]
pub struct NtfsScanner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtfsVolumeInfo {
    pub volume_serial_number: i64,
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_file_record: u32,
    pub mft_valid_data_length: i64,
    pub mft_start_lcn: i64,
}

#[derive(Debug, Error)]
pub enum NtfsScanError {
    #[error("direct NTFS scanning is only supported on Windows")]
    UnsupportedPlatform,
    #[error("direct NTFS MFT scan path is not complete yet")]
    MftScanIncomplete,
    #[error("MFT record 0 does not contain non-resident data runs")]
    MissingMftDataRuns,
    #[error("integer overflow while computing NTFS offsets")]
    IntegerOverflow,
    #[error("invalid volume `{0}`")]
    InvalidVolume(String),
    #[error(transparent)]
    Parse(#[from] MftParseError),
    #[error(transparent)]
    Graph(#[from] FileGraphError),
    #[cfg(windows)]
    #[error("{operation} failed for `{volume}`: {source}")]
    Windows {
        operation: &'static str,
        volume: String,
        source: windows::core::Error,
    },
}

impl NtfsScanner {
    #[cfg(windows)]
    pub fn scan_volume(volume: &str) -> Result<FileGraph, NtfsScanError> {
        let device_path = volume_device_path(volume)?;
        let handle = VolumeHandle::open(&device_path)?;
        let info = query_ntfs_volume_data(handle.raw(), &device_path)?;
        scan_mft(&handle, &info, volume, &device_path)
    }

    #[cfg(not(windows))]
    pub fn scan_volume(_: &str) -> Result<FileGraph, NtfsScanError> {
        Err(NtfsScanError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn probe_volume(volume: &str) -> Result<NtfsVolumeInfo, NtfsScanError> {
        let device_path = volume_device_path(volume)?;
        let handle = VolumeHandle::open(&device_path)?;
        query_ntfs_volume_data(handle.raw(), &device_path)
    }

    #[cfg(not(windows))]
    pub fn probe_volume(_: &str) -> Result<NtfsVolumeInfo, NtfsScanError> {
        Err(NtfsScanError::UnsupportedPlatform)
    }
}

#[derive(Debug, Clone)]
struct NtfsRawEntry {
    record_number: u64,
    parent_record_number: Option<u64>,
    name: String,
    kind: FileKind,
    size: u64,
    allocated: u64,
    modified_unix: i64,
    hard_links: u16,
}

impl fmt::Display for NtfsVolumeInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "serial={} bytes_per_cluster={} bytes_per_file_record={} mft_lcn={} mft_valid_data={}",
            self.volume_serial_number,
            self.bytes_per_cluster,
            self.bytes_per_file_record,
            self.mft_start_lcn,
            self.mft_valid_data_length
        )
    }
}

#[cfg(windows)]
struct VolumeHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl VolumeHandle {
    fn open(device_path: &str) -> Result<Self, NtfsScanError> {
        use windows::{
            Win32::Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ,
                FILE_SHARE_WRITE, OPEN_EXISTING,
            },
            core::PCWSTR,
        };

        let wide = to_wide(device_path);
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE;

        // SAFETY: `wide` is null-terminated, and all optional pointer parameters are absent.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_GENERIC_READ.0,
                share,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|source| NtfsScanError::Windows {
            operation: "CreateFileW",
            volume: device_path.to_owned(),
            source,
        })?;

        Ok(Self(handle))
    }

    fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        self.0
    }

    fn read_at(
        &self,
        offset: u64,
        buffer: &mut [u8],
        device_path: &str,
    ) -> Result<usize, NtfsScanError> {
        use windows::Win32::Storage::FileSystem::{FILE_BEGIN, ReadFile, SetFilePointerEx};

        let offset = i64::try_from(offset).map_err(|_| NtfsScanError::IntegerOverflow)?;
        // SAFETY: The handle is valid and the file pointer move uses an absolute non-negative
        // offset into the raw volume.
        unsafe { SetFilePointerEx(self.0, offset, None, FILE_BEGIN) }.map_err(|source| {
            NtfsScanError::Windows {
                operation: "SetFilePointerEx",
                volume: device_path.to_owned(),
                source,
            }
        })?;

        let mut bytes_read = 0_u32;
        // SAFETY: The buffer is valid mutable storage for the duration of the call.
        unsafe { ReadFile(self.0, Some(buffer), Some(&mut bytes_read), None) }.map_err(
            |source| NtfsScanError::Windows {
                operation: "ReadFile",
                volume: device_path.to_owned(),
                source,
            },
        )?;

        Ok(bytes_read as usize)
    }
}

#[cfg(windows)]
impl Drop for VolumeHandle {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;

        // SAFETY: The handle was returned by CreateFileW and is owned by this wrapper.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn query_ntfs_volume_data(
    handle: windows::Win32::Foundation::HANDLE,
    device_path: &str,
) -> Result<NtfsVolumeInfo, NtfsScanError> {
    use std::mem::{MaybeUninit, size_of};

    use windows::{
        Win32::System::{
            IO::DeviceIoControl,
            Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER},
        },
        core::Error,
    };

    let mut data = MaybeUninit::<NTFS_VOLUME_DATA_BUFFER>::zeroed();
    let mut bytes_returned = 0_u32;

    // SAFETY: The output buffer points to valid uninitialized storage for the exact Windows
    // structure requested by FSCTL_GET_NTFS_VOLUME_DATA. No input buffer is required.
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some(data.as_mut_ptr().cast()),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }
    .map_err(|source: Error| NtfsScanError::Windows {
        operation: "DeviceIoControl(FSCTL_GET_NTFS_VOLUME_DATA)",
        volume: device_path.to_owned(),
        source,
    })?;

    // SAFETY: DeviceIoControl succeeded and wrote the NTFS_VOLUME_DATA_BUFFER.
    let data = unsafe { data.assume_init() };

    Ok(NtfsVolumeInfo {
        volume_serial_number: data.VolumeSerialNumber,
        bytes_per_sector: data.BytesPerSector,
        bytes_per_cluster: data.BytesPerCluster,
        bytes_per_file_record: data.BytesPerFileRecordSegment,
        mft_valid_data_length: data.MftValidDataLength,
        mft_start_lcn: data.MftStartLcn,
    })
}

#[cfg(windows)]
fn scan_mft(
    handle: &VolumeHandle,
    info: &NtfsVolumeInfo,
    volume: &str,
    device_path: &str,
) -> Result<FileGraph, NtfsScanError> {
    let record_size = info.bytes_per_file_record as usize;
    let record0_offset = lcn_to_offset(info.mft_start_lcn, info.bytes_per_cluster)?;
    let record0 = read_record_at(handle, record0_offset, record_size, device_path)?;
    let mft_record = parse_file_record(&record0, info.bytes_per_sector)?;
    if mft_record.data_runs.is_empty() {
        return Err(NtfsScanError::MissingMftDataRuns);
    }

    let record_count = mft_record.data_size / u64::from(info.bytes_per_file_record);
    let entries = read_mft_entries(
        handle,
        info,
        device_path,
        &mft_record,
        record_count,
        record_size,
    )?;

    build_graph_from_entries(entries, root_display_name(volume))
}

#[cfg(windows)]
fn read_mft_entries(
    handle: &VolumeHandle,
    info: &NtfsVolumeInfo,
    device_path: &str,
    mft_record: &ParsedFileRecord,
    record_count: u64,
    record_size: usize,
) -> Result<HashMap<u64, NtfsRawEntry>, NtfsScanError> {
    let mut entries = HashMap::new();
    let mut record_number = 0_u64;

    for run in &mft_record.data_runs {
        let Some(lcn) = run.lcn else {
            record_number = record_number.saturating_add(
                run.clusters
                    .saturating_mul(u64::from(info.bytes_per_cluster))
                    / u64::from(info.bytes_per_file_record),
            );
            continue;
        };

        let run_offset = lcn_to_offset(lcn, info.bytes_per_cluster)?;
        let run_bytes = run
            .clusters
            .checked_mul(u64::from(info.bytes_per_cluster))
            .ok_or(NtfsScanError::IntegerOverflow)?;
        let records_in_run = run_bytes / u64::from(info.bytes_per_file_record);

        for idx in 0..records_in_run {
            if record_number >= record_count {
                return Ok(entries);
            }

            let offset = run_offset
                .checked_add(
                    idx.checked_mul(u64::from(info.bytes_per_file_record))
                        .ok_or(NtfsScanError::IntegerOverflow)?,
                )
                .ok_or(NtfsScanError::IntegerOverflow)?;
            let record = read_record_at(handle, offset, record_size, device_path)?;
            if let Ok(parsed) = parse_file_record(&record, info.bytes_per_sector)
                && let Some(entry) = raw_entry_from_record(record_number, &parsed)
            {
                entries.insert(record_number, entry);
            }

            record_number += 1;
        }
    }

    Ok(entries)
}

#[cfg(windows)]
fn read_record_at(
    handle: &VolumeHandle,
    offset: u64,
    record_size: usize,
    device_path: &str,
) -> Result<Vec<u8>, NtfsScanError> {
    let mut record = vec![0_u8; record_size];
    let bytes_read = handle.read_at(offset, &mut record, device_path)?;
    if bytes_read < record_size {
        record.truncate(bytes_read);
    }
    Ok(record)
}

#[cfg(windows)]
fn lcn_to_offset(lcn: i64, bytes_per_cluster: u32) -> Result<u64, NtfsScanError> {
    let lcn = u64::try_from(lcn).map_err(|_| NtfsScanError::IntegerOverflow)?;
    lcn.checked_mul(u64::from(bytes_per_cluster))
        .ok_or(NtfsScanError::IntegerOverflow)
}

fn raw_entry_from_record(record_number: u64, parsed: &ParsedFileRecord) -> Option<NtfsRawEntry> {
    if !parsed.header.is_in_use() || parsed.header.base_file_record != 0 {
        return None;
    }

    let name = best_file_name(&parsed.file_names)?;
    let is_directory = parsed.header.is_directory();
    let size = if is_directory {
        0
    } else if parsed.data_size > 0 {
        parsed.data_size
    } else {
        name.data_size
    };
    let allocated = if is_directory {
        0
    } else if parsed.allocated_size > 0 {
        parsed.allocated_size
    } else {
        name.allocated_size
    };

    Some(NtfsRawEntry {
        record_number,
        parent_record_number: (name.parent_record_number != record_number)
            .then_some(name.parent_record_number),
        name: name.name.clone(),
        kind: if is_directory {
            FileKind::Directory
        } else {
            FileKind::File
        },
        size,
        allocated,
        modified_unix: name.modified_unix,
        hard_links: parsed.header.hard_link_count,
    })
}

fn best_file_name(names: &[FileNameAttribute]) -> Option<&FileNameAttribute> {
    names.iter().max_by_key(|name| match name.namespace {
        1 | 3 => 3,
        0 => 2,
        2 => 1,
        _ => 0,
    })
}

fn build_graph_from_entries(
    entries: HashMap<u64, NtfsRawEntry>,
    root_name: String,
) -> Result<FileGraph, NtfsScanError> {
    let mut builder = FileGraphBuilder::new();
    let mut ids = HashMap::with_capacity(entries.len());
    let mut visiting = HashSet::new();

    let mut records: Vec<_> = entries.keys().copied().collect();
    records.sort_unstable();
    for record_number in records {
        add_entry_recursive(
            record_number,
            &entries,
            &mut builder,
            &mut ids,
            &mut visiting,
            &root_name,
        )?;
    }

    Ok(builder.finish())
}

fn add_entry_recursive(
    record_number: u64,
    entries: &HashMap<u64, NtfsRawEntry>,
    builder: &mut FileGraphBuilder,
    ids: &mut HashMap<u64, EntryId>,
    visiting: &mut HashSet<u64>,
    root_name: &str,
) -> Result<Option<EntryId>, NtfsScanError> {
    if let Some(id) = ids.get(&record_number) {
        return Ok(Some(*id));
    }
    let Some(entry) = entries.get(&record_number) else {
        return Ok(None);
    };
    if !visiting.insert(record_number) {
        return Ok(None);
    }

    let parent = entry
        .parent_record_number
        .and_then(|parent| {
            if parent == record_number {
                None
            } else {
                Some(parent)
            }
        })
        .map(|parent| add_entry_recursive(parent, entries, builder, ids, visiting, root_name))
        .transpose()?
        .flatten();

    let mut flags = EntryFlags::empty();
    if entry.hard_links > 1 {
        flags.insert(EntryFlags::HARD_LINK);
    }
    let name = if entry.record_number == ROOT_RECORD_NUMBER {
        root_name
    } else {
        &entry.name
    };
    let id = builder.add_entry_with_flags(
        parent,
        name,
        EntryMetadata {
            kind: entry.kind,
            size: entry.size,
            allocated: entry.allocated,
            modified_unix: entry.modified_unix,
            extra_flags: flags,
        },
    )?;
    ids.insert(record_number, id);
    visiting.remove(&record_number);

    Ok(Some(id))
}

fn root_display_name(volume: &str) -> String {
    let trimmed = volume.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return format!("{trimmed}\\");
    }
    if trimmed.is_empty() {
        volume.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(windows)]
fn volume_device_path(volume: &str) -> Result<String, NtfsScanError> {
    let trimmed = volume.trim_end_matches(['\\', '/']);
    if trimmed.starts_with(r"\\.\") {
        return Ok(trimmed.to_owned());
    }

    let mut chars = trimmed.chars();
    let Some(letter) = chars.next() else {
        return Err(NtfsScanError::InvalidVolume(volume.to_owned()));
    };
    let Some(':') = chars.next() else {
        return Err(NtfsScanError::InvalidVolume(volume.to_owned()));
    };
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return Err(NtfsScanError::InvalidVolume(volume.to_owned()));
    }

    Ok(format!(r"\\.\{}:", letter.to_ascii_uppercase()))
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{NtfsRawEntry, ROOT_RECORD_NUMBER, build_graph_from_entries};
    use diskloom_core::{EntryFlags, FileKind};

    #[cfg(windows)]
    use super::volume_device_path;

    #[cfg(windows)]
    #[test]
    fn volume_device_path_should_accept_drive_root() {
        assert_eq!(volume_device_path("c:\\").unwrap(), r"\\.\C:");
    }

    #[cfg(windows)]
    #[test]
    fn volume_device_path_should_accept_existing_device_path() {
        assert_eq!(volume_device_path(r"\\.\D:").unwrap(), r"\\.\D:");
    }

    #[test]
    fn build_graph_from_entries_should_reconstruct_parent_chain() {
        let mut entries = HashMap::new();
        entries.insert(
            ROOT_RECORD_NUMBER,
            NtfsRawEntry {
                record_number: ROOT_RECORD_NUMBER,
                parent_record_number: None,
                name: ".".to_owned(),
                kind: FileKind::Directory,
                size: 0,
                allocated: 0,
                modified_unix: 0,
                hard_links: 1,
            },
        );
        entries.insert(
            42,
            NtfsRawEntry {
                record_number: 42,
                parent_record_number: Some(ROOT_RECORD_NUMBER),
                name: "data.bin".to_owned(),
                kind: FileKind::File,
                size: 10,
                allocated: 16,
                modified_unix: 0,
                hard_links: 2,
            },
        );

        let graph = build_graph_from_entries(entries, "C:\\".to_owned()).unwrap();
        let file = graph
            .ids()
            .find(|id| graph.name(*id) == Some("data.bin"))
            .unwrap();

        assert_eq!(
            graph.reconstruct_path(file).unwrap().to_string_lossy(),
            "C:\\data.bin"
        );
        assert!(
            graph
                .entry(file)
                .unwrap()
                .flags
                .contains(EntryFlags::HARD_LINK)
        );
    }
}
