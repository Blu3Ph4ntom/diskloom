use diskloom_core::FileGraph;
use thiserror::Error;

#[derive(Debug, Default)]
pub struct NtfsScanner;

#[derive(Debug, Error)]
pub enum NtfsScanError {
    #[error("direct NTFS scanning is only supported on Windows")]
    UnsupportedPlatform,
    #[error("direct NTFS scanning is not implemented yet")]
    NotImplemented,
}

impl NtfsScanner {
    pub fn scan_volume(_: &str) -> Result<FileGraph, NtfsScanError> {
        Err(NtfsScanError::NotImplemented)
    }
}
