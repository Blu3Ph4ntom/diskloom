use std::time::Duration;
use std::{fmt, thread, time::Instant};

use diskloom_core::{
    EntryFlags, EntryId, EntryMetadata, FileGraph, FileGraphBuilder, FileGraphError, FileKind,
};
use thiserror::Error;

use crate::mft::{
    MftParseError, ParsedFileRecord, ScannedFileRecord, parse_file_record,
    parse_scanned_file_record_in_place,
};

const ROOT_RECORD_NUMBER: u64 = 5;
const EXTEND_RECORD_NUMBER: u64 = 11;
const RESERVED_METADATA_RECORDS: u64 = 16;
const MFT_READ_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const NO_PARENT_RECORD: u64 = u64::MAX;

#[derive(Debug, Default)]
pub struct NtfsScanner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NtfsScanProgress {
    pub records_read: u64,
    pub entries: u64,
    pub files: u64,
    pub directories: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtfsScanControl {
    Continue,
    Cancel,
}

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
    #[error("direct NTFS scan was cancelled")]
    Cancelled,
    #[error("direct NTFS scan worker panicked")]
    WorkerPanic,
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
        Self::scan_volume_with_progress(volume, 0, |_| {})
    }

    #[cfg(not(windows))]
    pub fn scan_volume(_: &str) -> Result<FileGraph, NtfsScanError> {
        Err(NtfsScanError::UnsupportedPlatform)
    }

    #[cfg(windows)]
    pub fn scan_volume_with_progress(
        volume: &str,
        progress_every: u64,
        mut on_progress: impl FnMut(NtfsScanProgress),
    ) -> Result<FileGraph, NtfsScanError> {
        Self::scan_volume_with_control(volume, progress_every, |progress| {
            on_progress(progress);
            NtfsScanControl::Continue
        })
    }

    #[cfg(windows)]
    pub fn scan_volume_with_control(
        volume: &str,
        progress_every: u64,
        on_progress: impl FnMut(NtfsScanProgress) -> NtfsScanControl,
    ) -> Result<FileGraph, NtfsScanError> {
        let device_path = volume_device_path(volume)?;
        let handle = VolumeHandle::open(&device_path)?;
        let info = query_ntfs_volume_data(handle.raw(), &device_path)?;
        scan_mft(
            &handle,
            &info,
            volume,
            &device_path,
            progress_every,
            on_progress,
        )
    }

    #[cfg(not(windows))]
    pub fn scan_volume_with_progress(
        _: &str,
        _: u64,
        _: impl FnMut(NtfsScanProgress),
    ) -> Result<FileGraph, NtfsScanError> {
        Err(NtfsScanError::UnsupportedPlatform)
    }

    #[cfg(not(windows))]
    pub fn scan_volume_with_control(
        _: &str,
        _: u64,
        _: impl FnMut(NtfsScanProgress) -> NtfsScanControl,
    ) -> Result<FileGraph, NtfsScanError> {
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
    parent_record_number: Option<u64>,
    name: String,
    kind: FileKind,
    size: u64,
    allocated: u64,
    modified_unix: i64,
    hard_links: u16,
}

#[derive(Debug)]
struct NtfsRawEntries {
    present: Vec<u8>,
    parent_record_numbers: Vec<u64>,
    names: Vec<Option<String>>,
    kinds: Vec<FileKind>,
    sizes: Vec<u64>,
    allocated: Vec<u64>,
    modified_unix: Vec<i64>,
    hard_links: Vec<u16>,
}

impl NtfsRawEntries {
    fn with_len(len: usize) -> Self {
        let mut names = Vec::with_capacity(len);
        names.resize_with(len, || None);
        Self {
            present: vec![0; len],
            parent_record_numbers: vec![NO_PARENT_RECORD; len],
            names,
            kinds: vec![FileKind::File; len],
            sizes: vec![0; len],
            allocated: vec![0; len],
            modified_unix: vec![0; len],
            hard_links: vec![0; len],
        }
    }

    fn len(&self) -> usize {
        self.present.len()
    }

    fn is_present(&self, record_number: usize) -> bool {
        self.present.get(record_number).copied() == Some(1)
    }

    #[cfg(test)]
    fn insert(&mut self, record_number: usize, entry: NtfsRawEntry) {
        if record_number >= self.len() {
            return;
        }
        self.present[record_number] = 1;
        self.parent_record_numbers[record_number] =
            entry.parent_record_number.unwrap_or(NO_PARENT_RECORD);
        self.names[record_number] = Some(entry.name);
        self.kinds[record_number] = entry.kind;
        self.sizes[record_number] = entry.size;
        self.allocated[record_number] = entry.allocated;
        self.modified_unix[record_number] = entry.modified_unix;
        self.hard_links[record_number] = entry.hard_links;
    }

    fn parent_record_number(&self, record_number: usize) -> Option<u64> {
        let parent = *self.parent_record_numbers.get(record_number)?;
        (parent != NO_PARENT_RECORD).then_some(parent)
    }

    fn columns_mut(&mut self, start: usize, len: usize) -> NtfsRawEntryColumnsMut<'_> {
        let end = start + len;
        NtfsRawEntryColumnsMut {
            present: &mut self.present[start..end],
            parent_record_numbers: &mut self.parent_record_numbers[start..end],
            names: &mut self.names[start..end],
            kinds: &mut self.kinds[start..end],
            sizes: &mut self.sizes[start..end],
            allocated: &mut self.allocated[start..end],
            modified_unix: &mut self.modified_unix[start..end],
            hard_links: &mut self.hard_links[start..end],
        }
    }
}

struct NtfsRawEntryColumnsMut<'a> {
    present: &'a mut [u8],
    parent_record_numbers: &'a mut [u64],
    names: &'a mut [Option<String>],
    kinds: &'a mut [FileKind],
    sizes: &'a mut [u64],
    allocated: &'a mut [u64],
    modified_unix: &'a mut [i64],
    hard_links: &'a mut [u16],
}

impl<'a> NtfsRawEntryColumnsMut<'a> {
    fn split_at_mut(self, mid: usize) -> (Self, Self) {
        let (present_left, present_right) = self.present.split_at_mut(mid);
        let (parent_left, parent_right) = self.parent_record_numbers.split_at_mut(mid);
        let (name_left, name_right) = self.names.split_at_mut(mid);
        let (kind_left, kind_right) = self.kinds.split_at_mut(mid);
        let (size_left, size_right) = self.sizes.split_at_mut(mid);
        let (allocated_left, allocated_right) = self.allocated.split_at_mut(mid);
        let (modified_left, modified_right) = self.modified_unix.split_at_mut(mid);
        let (hard_left, hard_right) = self.hard_links.split_at_mut(mid);
        (
            Self {
                present: present_left,
                parent_record_numbers: parent_left,
                names: name_left,
                kinds: kind_left,
                sizes: size_left,
                allocated: allocated_left,
                modified_unix: modified_left,
                hard_links: hard_left,
            },
            Self {
                present: present_right,
                parent_record_numbers: parent_right,
                names: name_right,
                kinds: kind_right,
                sizes: size_right,
                allocated: allocated_right,
                modified_unix: modified_right,
                hard_links: hard_right,
            },
        )
    }

    fn insert_at(&mut self, idx: usize, entry: NtfsRawEntry) {
        self.present[idx] = 1;
        self.parent_record_numbers[idx] = entry.parent_record_number.unwrap_or(NO_PARENT_RECORD);
        self.names[idx] = Some(entry.name);
        self.kinds[idx] = entry.kind;
        self.sizes[idx] = entry.size;
        self.allocated[idx] = entry.allocated;
        self.modified_unix[idx] = entry.modified_unix;
        self.hard_links[idx] = entry.hard_links;
    }
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
    progress_every: u64,
    on_progress: impl FnMut(NtfsScanProgress) -> NtfsScanControl,
) -> Result<FileGraph, NtfsScanError> {
    let total_started = Instant::now();
    let record_size = info.bytes_per_file_record as usize;
    let record0_started = Instant::now();
    let record0_offset = lcn_to_offset(info.mft_start_lcn, info.bytes_per_cluster)?;
    let record0 = read_record_at(handle, record0_offset, record_size, device_path)?;
    let mft_record = parse_file_record(&record0, info.bytes_per_sector)?;
    if mft_record.data_runs.is_empty() {
        return Err(NtfsScanError::MissingMftDataRuns);
    }
    trace_ntfs_phase("record0", record0_started.elapsed());

    let record_count = mft_record.data_size / u64::from(info.bytes_per_file_record);
    let read_plan = MftReadPlan {
        info,
        device_path,
        mft_record: &mft_record,
        record_count,
        record_size,
        progress_every,
    };
    let read_started = Instant::now();
    let entries = read_mft_entries(handle, read_plan, on_progress)?;
    trace_ntfs_phase("read_entries", read_started.elapsed());

    let build_started = Instant::now();
    let graph = build_graph_from_entries(entries, root_display_name(volume))?;
    trace_ntfs_phase("build_graph", build_started.elapsed());
    trace_ntfs_phase("total", total_started.elapsed());
    Ok(graph)
}

#[cfg(windows)]
struct MftReadPlan<'a> {
    info: &'a NtfsVolumeInfo,
    device_path: &'a str,
    mft_record: &'a ParsedFileRecord,
    record_count: u64,
    record_size: usize,
    progress_every: u64,
}

#[cfg(windows)]
fn read_mft_entries(
    handle: &VolumeHandle,
    plan: MftReadPlan<'_>,
    mut on_progress: impl FnMut(NtfsScanProgress) -> NtfsScanControl,
) -> Result<NtfsRawEntries, NtfsScanError> {
    let entry_capacity =
        usize::try_from(plan.record_count).map_err(|_| NtfsScanError::IntegerOverflow)?;
    let mut entries = NtfsRawEntries::with_len(entry_capacity);
    let mut record_number = 0_u64;
    let mut progress = NtfsScanProgress::default();
    let mut last_progress_records = 0;
    let records_per_chunk = records_per_mft_chunk(plan.record_size) as u64;
    let mut buffer = vec![0_u8; records_per_chunk as usize * plan.record_size];
    let mut read_elapsed = Duration::ZERO;
    let mut parse_elapsed = Duration::ZERO;

    for run in &plan.mft_record.data_runs {
        let Some(lcn) = run.lcn else {
            record_number = record_number.saturating_add(
                run.clusters
                    .saturating_mul(u64::from(plan.info.bytes_per_cluster))
                    / u64::from(plan.info.bytes_per_file_record),
            );
            continue;
        };

        let run_offset = lcn_to_offset(lcn, plan.info.bytes_per_cluster)?;
        let run_bytes = run
            .clusters
            .checked_mul(u64::from(plan.info.bytes_per_cluster))
            .ok_or(NtfsScanError::IntegerOverflow)?;
        let records_in_run = run_bytes / u64::from(plan.info.bytes_per_file_record);
        let mut run_record_idx = 0_u64;

        while run_record_idx < records_in_run {
            if record_number >= plan.record_count {
                trace_ntfs_phase("read_raw", read_elapsed);
                trace_ntfs_phase("parse_records", parse_elapsed);
                emit_ntfs_progress(
                    progress,
                    plan.progress_every,
                    &mut last_progress_records,
                    &mut on_progress,
                )?;
                return Ok(entries);
            }

            let remaining_run_records = records_in_run - run_record_idx;
            let remaining_mft_records = plan.record_count - record_number;
            let chunk_records = records_per_chunk
                .min(remaining_run_records)
                .min(remaining_mft_records);
            let chunk_bytes = chunk_records
                .checked_mul(plan.record_size as u64)
                .and_then(|bytes| usize::try_from(bytes).ok())
                .ok_or(NtfsScanError::IntegerOverflow)?;
            let offset = run_offset
                .checked_add(
                    run_record_idx
                        .checked_mul(u64::from(plan.info.bytes_per_file_record))
                        .ok_or(NtfsScanError::IntegerOverflow)?,
                )
                .ok_or(NtfsScanError::IntegerOverflow)?;
            let read_started = Instant::now();
            let bytes_read =
                handle.read_at(offset, &mut buffer[..chunk_bytes], plan.device_path)?;
            read_elapsed += read_started.elapsed();
            let records_read = bytes_read / plan.record_size;
            if records_read == 0 {
                trace_ntfs_phase("read_raw", read_elapsed);
                trace_ntfs_phase("parse_records", parse_elapsed);
                emit_ntfs_progress(
                    progress,
                    plan.progress_every,
                    &mut last_progress_records,
                    &mut on_progress,
                )?;
                return Ok(entries);
            }

            let parse_started = Instant::now();
            let chunk = process_mft_record_chunk(
                record_number,
                records_read,
                plan.record_size,
                plan.info.bytes_per_sector,
                &mut buffer[..records_read * plan.record_size],
                &mut entries,
            )?;
            parse_elapsed += parse_started.elapsed();
            progress.records_read = progress.records_read.saturating_add(chunk.records_read);
            progress.entries = progress.entries.saturating_add(chunk.entries);
            progress.files = progress.files.saturating_add(chunk.files);
            progress.directories = progress.directories.saturating_add(chunk.directories);
            progress.skipped = progress.skipped.saturating_add(chunk.skipped);
            maybe_emit_ntfs_progress(
                progress,
                plan.progress_every,
                &mut last_progress_records,
                &mut on_progress,
            )?;

            record_number += records_read as u64;
            run_record_idx += records_read as u64;
        }
    }

    emit_ntfs_progress(
        progress,
        plan.progress_every,
        &mut last_progress_records,
        &mut on_progress,
    )?;
    trace_ntfs_phase("read_raw", read_elapsed);
    trace_ntfs_phase("parse_records", parse_elapsed);
    Ok(entries)
}

fn records_per_mft_chunk(record_size: usize) -> usize {
    if record_size == 0 {
        return 1;
    }
    (MFT_READ_CHUNK_BYTES / record_size).max(1)
}

#[derive(Debug, Default)]
struct NtfsChunkResult {
    records_read: u64,
    entries: u64,
    files: u64,
    directories: u64,
    skipped: u64,
}

impl NtfsChunkResult {
    fn count_entry(&mut self, kind: FileKind) {
        self.entries += 1;
        if kind == FileKind::Directory {
            self.directories += 1;
        } else {
            self.files += 1;
        }
    }

    fn merge(&mut self, other: Self) {
        self.records_read = self.records_read.saturating_add(other.records_read);
        self.entries = self.entries.saturating_add(other.entries);
        self.files = self.files.saturating_add(other.files);
        self.directories = self.directories.saturating_add(other.directories);
        self.skipped = self.skipped.saturating_add(other.skipped);
    }
}

fn process_mft_record_chunk(
    first_record_number: u64,
    records_read: usize,
    record_size: usize,
    bytes_per_sector: u32,
    buffer: &mut [u8],
    entries: &mut NtfsRawEntries,
) -> Result<NtfsChunkResult, NtfsScanError> {
    let entry_start =
        usize::try_from(first_record_number).map_err(|_| NtfsScanError::IntegerOverflow)?;
    let columns = entries.columns_mut(entry_start, records_read);
    let workers = ntfs_parse_workers(records_read);
    if workers <= 1 {
        return Ok(process_mft_record_range(
            first_record_number,
            records_read,
            record_size,
            bytes_per_sector,
            buffer,
            columns,
        ));
    }

    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        let records_per_worker = records_read.div_ceil(workers);
        let mut remaining = buffer;
        let mut remaining_columns = columns;
        let mut next_record = first_record_number;
        let mut remaining_records = records_read;

        while remaining_records > 0 {
            let worker_records = records_per_worker.min(remaining_records);
            let worker_bytes = worker_records * record_size;
            let (worker_buffer, rest) = remaining.split_at_mut(worker_bytes);
            remaining = rest;
            let (worker_columns, rest_columns) = remaining_columns.split_at_mut(worker_records);
            remaining_columns = rest_columns;
            let worker_first_record = next_record;
            handles.push(scope.spawn(move || {
                process_mft_record_range(
                    worker_first_record,
                    worker_records,
                    record_size,
                    bytes_per_sector,
                    worker_buffer,
                    worker_columns,
                )
            }));
            next_record += worker_records as u64;
            remaining_records -= worker_records;
        }

        let mut chunk = NtfsChunkResult::default();
        for handle in handles {
            let worker_chunk = handle.join().map_err(|_| NtfsScanError::WorkerPanic)?;
            chunk.merge(worker_chunk);
        }
        Ok(chunk)
    })
}

fn process_mft_record_range(
    first_record_number: u64,
    records_read: usize,
    record_size: usize,
    bytes_per_sector: u32,
    buffer: &mut [u8],
    mut columns: NtfsRawEntryColumnsMut<'_>,
) -> NtfsChunkResult {
    let mut chunk = NtfsChunkResult::default();

    for record_idx in 0..records_read {
        let start = record_idx * record_size;
        let record = &mut buffer[start..start + record_size];
        chunk.records_read += 1;
        if let Some(entry) = parse_mft_record_entry(
            first_record_number + record_idx as u64,
            record,
            bytes_per_sector,
        ) {
            let kind = entry.kind;
            columns.insert_at(record_idx, entry);
            chunk.count_entry(kind);
        } else {
            chunk.skipped += 1;
        }
    }

    chunk
}

fn ntfs_parse_workers(records_read: usize) -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(12)
        .min(records_read.max(1))
}

#[cfg(test)]
fn process_mft_record(
    record_number: u64,
    record: &mut [u8],
    bytes_per_sector: u32,
    progress: &mut NtfsScanProgress,
) -> Option<NtfsRawEntry> {
    progress.records_read += 1;
    if let Some(entry) = parse_mft_record_entry(record_number, record, bytes_per_sector) {
        progress.entries += 1;
        if entry.kind == FileKind::Directory {
            progress.directories += 1;
        } else {
            progress.files += 1;
        }
        Some(entry)
    } else {
        progress.skipped += 1;
        None
    }
}

fn parse_mft_record_entry(
    record_number: u64,
    record: &mut [u8],
    bytes_per_sector: u32,
) -> Option<NtfsRawEntry> {
    let parsed = parse_scanned_file_record_in_place(record, bytes_per_sector).ok()?;
    raw_entry_from_scanned_record(record_number, parsed)
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

fn raw_entry_from_scanned_record(
    record_number: u64,
    parsed: ScannedFileRecord,
) -> Option<NtfsRawEntry> {
    if !parsed.header.is_in_use() || parsed.header.base_file_record != 0 {
        return None;
    }
    if is_reserved_metadata_record(record_number) {
        return None;
    }

    let name = parsed.file_name?;
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
        parent_record_number: (name.parent_record_number != record_number)
            .then_some(name.parent_record_number),
        name: name.name,
        kind: if is_directory {
            FileKind::Directory
        } else {
            FileKind::File
        },
        size,
        allocated,
        modified_unix: parsed.standard_modified_unix.unwrap_or(name.modified_unix),
        hard_links: parsed.header.hard_link_count,
    })
}

fn build_graph_from_entries(
    mut entries: NtfsRawEntries,
    root_name: String,
) -> Result<FileGraph, NtfsScanError> {
    let mut builder = FileGraphBuilder::without_name_dedup();
    let mut ids = vec![None; entries.len()];
    let mut visiting = vec![false; entries.len()];

    let add_started = Instant::now();
    if ROOT_RECORD_NUMBER < entries.len() as u64 {
        add_entry_recursive(
            ROOT_RECORD_NUMBER as usize,
            &mut entries,
            &mut builder,
            &mut ids,
            &mut visiting,
            &root_name,
        )?;
    }
    for record_number in 0..entries.len() {
        if !entries.is_present(record_number) {
            continue;
        }
        add_entry_recursive(
            record_number,
            &mut entries,
            &mut builder,
            &mut ids,
            &mut visiting,
            &root_name,
        )?;
    }
    trace_ntfs_phase("graph_add", add_started.elapsed());

    let finish_started = Instant::now();
    let graph = builder.finish();
    trace_ntfs_phase("graph_finish", finish_started.elapsed());
    Ok(graph)
}

fn add_entry_recursive(
    record_number: usize,
    entries: &mut NtfsRawEntries,
    builder: &mut FileGraphBuilder,
    ids: &mut [Option<EntryId>],
    visiting: &mut [bool],
    root_name: &str,
) -> Result<Option<EntryId>, NtfsScanError> {
    if let Some(id) = ids[record_number] {
        return Ok(Some(id));
    }
    if !entries.is_present(record_number) {
        return Ok(None);
    }
    if visiting[record_number] {
        return Ok(None);
    }
    visiting[record_number] = true;

    let parent_record_number = entries.parent_record_number(record_number);
    let parent = match parent_record_number {
        Some(parent) if is_reserved_metadata_record(parent) => {
            visiting[record_number] = false;
            return Ok(None);
        }
        Some(parent) if parent != record_number as u64 => {
            let parent = usize::try_from(parent)
                .ok()
                .filter(|parent| *parent < entries.len());
            if let Some(parent) = parent.filter(|parent| entries.is_present(*parent)) {
                add_entry_recursive(parent, entries, builder, ids, visiting, root_name)?
            } else {
                fallback_root_parent(record_number, ids)
            }
        }
        Some(_) | None => fallback_root_parent(record_number, ids),
    };

    let mut flags = EntryFlags::empty();
    if entries.hard_links[record_number] > 1 {
        flags.insert(EntryFlags::HARD_LINK);
    }
    let name = if record_number as u64 == ROOT_RECORD_NUMBER {
        root_name.to_owned()
    } else {
        entries.names[record_number].take().unwrap_or_default()
    };
    let id = builder.add_entry_with_flags_owned_name(
        parent,
        name,
        EntryMetadata {
            kind: entries.kinds[record_number],
            size: entries.sizes[record_number],
            allocated: entries.allocated[record_number],
            modified_unix: entries.modified_unix[record_number],
            extra_flags: flags,
        },
    )?;
    ids[record_number] = Some(id);
    visiting[record_number] = false;

    Ok(Some(id))
}

fn fallback_root_parent(record_number: usize, ids: &[Option<EntryId>]) -> Option<EntryId> {
    if record_number as u64 == ROOT_RECORD_NUMBER {
        return None;
    }
    ids.get(ROOT_RECORD_NUMBER as usize).copied().flatten()
}

fn is_reserved_metadata_record(record_number: u64) -> bool {
    record_number < RESERVED_METADATA_RECORDS
        && record_number != ROOT_RECORD_NUMBER
        && record_number != EXTEND_RECORD_NUMBER
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

fn trace_ntfs_phase(label: &str, elapsed: std::time::Duration) {
    let Ok(path) = std::env::var("DISKLOOM_NTFS_TRACE_FILE") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    use std::io::Write;
    let _ = writeln!(file, "{label},{}", elapsed.as_millis());
}

fn maybe_emit_ntfs_progress(
    progress: NtfsScanProgress,
    progress_every: u64,
    last_records: &mut u64,
    on_progress: &mut impl FnMut(NtfsScanProgress) -> NtfsScanControl,
) -> Result<(), NtfsScanError> {
    if progress_every == 0 {
        return Ok(());
    }
    if progress.records_read == 1 || progress.records_read.is_multiple_of(progress_every) {
        emit_ntfs_progress(progress, progress_every, last_records, on_progress)?;
    }
    Ok(())
}

fn emit_ntfs_progress(
    progress: NtfsScanProgress,
    progress_every: u64,
    last_records: &mut u64,
    on_progress: &mut impl FnMut(NtfsScanProgress) -> NtfsScanControl,
) -> Result<(), NtfsScanError> {
    if progress_every == 0 || progress.records_read == *last_records {
        return Ok(());
    }
    *last_records = progress.records_read;
    match on_progress(progress) {
        NtfsScanControl::Continue => Ok(()),
        NtfsScanControl::Cancel => Err(NtfsScanError::Cancelled),
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
    use super::{
        NtfsRawEntries, NtfsRawEntry, NtfsScanControl, NtfsScanError, NtfsScanProgress,
        ROOT_RECORD_NUMBER, build_graph_from_entries, emit_ntfs_progress, maybe_emit_ntfs_progress,
        process_mft_record, raw_entry_from_scanned_record, records_per_mft_chunk,
    };
    use crate::mft::{FileRecordHeader, ScannedFileNameAttribute, ScannedFileRecord};
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
        let mut entries = NtfsRawEntries::with_len(43);
        entries.insert(
            ROOT_RECORD_NUMBER as usize,
            NtfsRawEntry {
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

    #[test]
    fn build_graph_from_entries_should_attach_orphans_to_volume_root() {
        let mut entries = NtfsRawEntries::with_len(43);
        entries.insert(
            ROOT_RECORD_NUMBER as usize,
            NtfsRawEntry {
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
                parent_record_number: Some(999),
                name: "orphan.bin".to_owned(),
                kind: FileKind::File,
                size: 10,
                allocated: 16,
                modified_unix: 0,
                hard_links: 1,
            },
        );

        let graph = build_graph_from_entries(entries, "C:\\".to_owned()).unwrap();
        let root_count = graph
            .ids()
            .filter(|id| graph.entry(*id).is_some_and(|entry| entry.parent.is_none()))
            .count();
        let orphan = graph
            .ids()
            .find(|id| graph.name(*id) == Some("orphan.bin"))
            .unwrap();

        assert_eq!(root_count, 1);
        assert_eq!(
            graph.reconstruct_path(orphan).unwrap().to_string_lossy(),
            "C:\\orphan.bin"
        );
    }

    #[test]
    fn build_graph_from_entries_should_skip_children_under_reserved_metadata() {
        let mut entries = NtfsRawEntries::with_len(43);
        entries.insert(
            ROOT_RECORD_NUMBER as usize,
            NtfsRawEntry {
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
                parent_record_number: Some(8),
                name: "$BadClus".to_owned(),
                kind: FileKind::File,
                size: 500_000_000_000,
                allocated: 500_000_000_000,
                modified_unix: 0,
                hard_links: 1,
            },
        );

        let graph = build_graph_from_entries(entries, "C:\\".to_owned()).unwrap();

        assert!(graph.ids().all(|id| graph.name(id) != Some("$BadClus")));
        assert_eq!(graph.len(), 1);
    }

    #[test]
    fn raw_entry_from_scanned_record_should_skip_reserved_metadata_records() {
        let parsed = ScannedFileRecord {
            header: FileRecordHeader {
                sequence_number: 1,
                hard_link_count: 1,
                first_attribute_offset: 56,
                flags: 0x0001,
                bytes_in_use: 0,
                bytes_allocated: 0,
                base_file_record: 0,
                next_attribute_id: 0,
                record_number: 8,
            },
            standard_modified_unix: Some(200),
            file_name: Some(ScannedFileNameAttribute {
                parent_record_number: ROOT_RECORD_NUMBER,
                allocated_size: 500_000_000_000,
                data_size: 500_000_000_000,
                modified_unix: 100,
                name: "$BadClus".to_owned(),
            }),
            data_size: 500_000_000_000,
            allocated_size: 500_000_000_000,
        };

        assert!(raw_entry_from_scanned_record(8, parsed).is_none());
    }

    #[test]
    fn raw_entry_from_scanned_record_should_prefer_standard_information_modified_time() {
        let parsed = ScannedFileRecord {
            header: FileRecordHeader {
                sequence_number: 1,
                hard_link_count: 1,
                first_attribute_offset: 56,
                flags: 0x0001,
                bytes_in_use: 0,
                bytes_allocated: 0,
                base_file_record: 0,
                next_attribute_id: 0,
                record_number: 42,
            },
            standard_modified_unix: Some(200),
            file_name: Some(ScannedFileNameAttribute {
                parent_record_number: ROOT_RECORD_NUMBER,
                allocated_size: 16,
                data_size: 10,
                modified_unix: 100,
                name: "data.bin".to_owned(),
            }),
            data_size: 10,
            allocated_size: 16,
        };

        let entry = raw_entry_from_scanned_record(42, parsed).unwrap();

        assert_eq!(entry.modified_unix, 200);
    }

    #[test]
    fn ntfs_progress_should_emit_first_interval_and_final_snapshots() {
        let mut last_records = 0;
        let mut snapshots = Vec::new();
        let mut collect = |progress: NtfsScanProgress| {
            snapshots.push(progress.records_read);
            NtfsScanControl::Continue
        };

        maybe_emit_ntfs_progress(
            NtfsScanProgress {
                records_read: 1,
                entries: 1,
                files: 1,
                directories: 0,
                skipped: 0,
            },
            4,
            &mut last_records,
            &mut collect,
        )
        .unwrap();
        maybe_emit_ntfs_progress(
            NtfsScanProgress {
                records_read: 3,
                entries: 2,
                files: 2,
                directories: 0,
                skipped: 1,
            },
            4,
            &mut last_records,
            &mut collect,
        )
        .unwrap();
        maybe_emit_ntfs_progress(
            NtfsScanProgress {
                records_read: 4,
                entries: 3,
                files: 2,
                directories: 1,
                skipped: 1,
            },
            4,
            &mut last_records,
            &mut collect,
        )
        .unwrap();
        emit_ntfs_progress(
            NtfsScanProgress {
                records_read: 5,
                entries: 4,
                files: 3,
                directories: 1,
                skipped: 1,
            },
            4,
            &mut last_records,
            &mut collect,
        )
        .unwrap();

        assert_eq!(snapshots, vec![1, 4, 5]);
    }

    #[test]
    fn ntfs_progress_should_return_cancelled_when_callback_cancels() {
        let mut last_records = 0;
        let mut cancel = |_| NtfsScanControl::Cancel;

        let error = emit_ntfs_progress(
            NtfsScanProgress {
                records_read: 1,
                entries: 1,
                files: 1,
                directories: 0,
                skipped: 0,
            },
            1,
            &mut last_records,
            &mut cancel,
        )
        .unwrap_err();

        assert!(matches!(error, NtfsScanError::Cancelled));
    }

    #[test]
    fn records_per_mft_chunk_should_batch_records_with_floor_of_one() {
        assert_eq!(records_per_mft_chunk(1024), 8_192);
        assert_eq!(records_per_mft_chunk(16 * 1024 * 1024), 1);
        assert_eq!(records_per_mft_chunk(0), 1);
    }

    #[test]
    fn process_mft_record_should_count_parse_failures_as_skipped() {
        let mut progress = NtfsScanProgress::default();
        let mut record = [0_u8; 64];

        let entry = process_mft_record(42, &mut record, 512, &mut progress);

        assert!(entry.is_none());
        assert_eq!(progress.records_read, 1);
        assert_eq!(progress.skipped, 1);
    }
}
