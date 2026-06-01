#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeKind {
    Ntfs,
    Other(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub root: String,
    pub kind: VolumeKind,
}

#[derive(Debug, thiserror::Error)]
pub enum WindowsVolumeError {
    #[error("Windows volume discovery is only supported on Windows")]
    UnsupportedPlatform,
}

#[cfg(windows)]
pub fn discover_volumes() -> Result<Vec<VolumeInfo>, WindowsVolumeError> {
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn discover_volumes() -> Result<Vec<VolumeInfo>, WindowsVolumeError> {
    Err(WindowsVolumeError::UnsupportedPlatform)
}
