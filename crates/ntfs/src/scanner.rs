use std::fmt;

use diskloom_core::FileGraph;
use thiserror::Error;

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
    #[error("invalid volume `{0}`")]
    InvalidVolume(String),
    #[cfg(windows)]
    #[error("{operation} failed for `{volume}`: {source}")]
    Windows {
        operation: &'static str,
        volume: String,
        source: windows::core::Error,
    },
}

impl NtfsScanner {
    pub fn scan_volume(volume: &str) -> Result<FileGraph, NtfsScanError> {
        let _ = Self::probe_volume(volume)?;
        Err(NtfsScanError::MftScanIncomplete)
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

#[cfg(all(test, windows))]
mod tests {
    use super::volume_device_path;

    #[test]
    fn volume_device_path_should_accept_drive_root() {
        assert_eq!(volume_device_path("c:\\").unwrap(), r"\\.\C:");
    }

    #[test]
    fn volume_device_path_should_accept_existing_device_path() {
        assert_eq!(volume_device_path(r"\\.\D:").unwrap(), r"\\.\D:");
    }
}
