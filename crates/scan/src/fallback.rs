use std::path::PathBuf;

use diskloom_core::FileGraph;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub root: PathBuf,
    pub follow_symlinks: bool,
}

impl ScanOptions {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            follow_symlinks: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanSummary {
    pub entries: u64,
    pub inaccessible: u64,
}

#[derive(Debug)]
pub struct FallbackScanner;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("fallback scanner is not implemented yet")]
    NotImplemented,
}

impl FallbackScanner {
    pub fn scan(_: ScanOptions) -> Result<(FileGraph, ScanSummary), ScanError> {
        Err(ScanError::NotImplemented)
    }
}
