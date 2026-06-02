use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ELEVATED_SCAN_TASK_NAME: &str = "DiskLoomElevatedScan";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElevatedScanRequest {
    pub nonce: String,
    pub path: String,
    pub snapshot_path: PathBuf,
    pub error_path: PathBuf,
}

impl ElevatedScanRequest {
    pub fn new(path: &Path) -> Result<Self, ScanBrokerError> {
        let dir = elevated_scan_dir()?;
        fs::create_dir_all(&dir)?;
        let nonce = request_nonce();
        Ok(Self {
            snapshot_path: dir.join(format!("{nonce}.dlsnap")),
            error_path: dir.join(format!("{nonce}.err")),
            nonce,
            path: path.to_string_lossy().into_owned(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ScanBrokerError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("scheduled task command failed: {0}")]
    CommandFailed(String),
    #[error("broker output paths are outside the DiskLoom broker directory")]
    InvalidOutputPath,
    #[error("administrator scan task is only available on Windows")]
    UnsupportedPlatform,
}

pub fn elevated_scan_request_path() -> Result<PathBuf, ScanBrokerError> {
    Ok(elevated_scan_dir()?.join("request.json"))
}

pub fn write_elevated_scan_request(request: &ElevatedScanRequest) -> Result<(), ScanBrokerError> {
    validate_elevated_scan_output_paths(request)?;
    let path = elevated_scan_request_path()?;
    let bytes = serde_json::to_vec(request)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn read_elevated_scan_request() -> Result<ElevatedScanRequest, ScanBrokerError> {
    let path = elevated_scan_request_path()?;
    let bytes = fs::read(path)?;
    let request = serde_json::from_slice(&bytes)?;
    validate_elevated_scan_output_paths(&request)?;
    Ok(request)
}

pub fn remove_elevated_scan_request(request: &ElevatedScanRequest) {
    if let Ok(path) = elevated_scan_request_path() {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(&request.snapshot_path);
    let _ = fs::remove_file(&request.error_path);
}

pub fn validate_elevated_scan_output_paths(
    request: &ElevatedScanRequest,
) -> Result<(), ScanBrokerError> {
    let dir = elevated_scan_dir()?;
    if is_direct_child_of(&request.snapshot_path, &dir)
        && is_direct_child_of(&request.error_path, &dir)
    {
        return Ok(());
    }
    Err(ScanBrokerError::InvalidOutputPath)
}

#[cfg(windows)]
pub fn register_elevated_scan_task(worker_exe: &Path) -> Result<(), ScanBrokerError> {
    let worker_exe = worker_exe.canonicalize()?;
    let task_action = format!("\"{}\" --broker-worker", worker_exe.display());
    run_schtasks([
        "/Create",
        "/TN",
        ELEVATED_SCAN_TASK_NAME,
        "/TR",
        &task_action,
        "/SC",
        "ONCE",
        "/ST",
        "00:00",
        "/RL",
        "HIGHEST",
        "/F",
    ])
}

#[cfg(not(windows))]
pub fn register_elevated_scan_task(_: &Path) -> Result<(), ScanBrokerError> {
    Err(ScanBrokerError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn unregister_elevated_scan_task() -> Result<(), ScanBrokerError> {
    run_schtasks(["/Delete", "/TN", ELEVATED_SCAN_TASK_NAME, "/F"])
}

#[cfg(not(windows))]
pub fn unregister_elevated_scan_task() -> Result<(), ScanBrokerError> {
    Err(ScanBrokerError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn run_elevated_scan_task() -> Result<(), ScanBrokerError> {
    run_schtasks(["/Run", "/TN", ELEVATED_SCAN_TASK_NAME])
}

#[cfg(not(windows))]
pub fn run_elevated_scan_task() -> Result<(), ScanBrokerError> {
    Err(ScanBrokerError::UnsupportedPlatform)
}

#[cfg(windows)]
fn run_schtasks<const N: usize>(args: [&str; N]) -> Result<(), ScanBrokerError> {
    let output = Command::new("schtasks").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(ScanBrokerError::CommandFailed(message))
}

fn elevated_scan_dir() -> Result<PathBuf, ScanBrokerError> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(base.join("DiskLoom").join("Broker"))
}

fn request_nonce() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    format!("{}-{millis}", std::process::id())
}

fn is_direct_child_of(path: &Path, parent: &Path) -> bool {
    path.parent()
        .is_some_and(|candidate| normalize_for_compare(candidate) == normalize_for_compare(parent))
}

fn normalize_for_compare(path: &Path) -> String {
    path.to_string_lossy()
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::is_direct_child_of;

    #[test]
    fn direct_child_check_should_accept_file_in_parent() {
        assert!(is_direct_child_of(
            Path::new(r"C:\Users\a\AppData\Local\DiskLoom\Broker\out.dlsnap"),
            Path::new(r"c:\users\a\appdata\local\diskloom\broker\")
        ));
    }

    #[test]
    fn direct_child_check_should_reject_nested_file() {
        assert!(!is_direct_child_of(
            Path::new(r"C:\Users\a\AppData\Local\DiskLoom\Broker\nested\out.dlsnap"),
            Path::new(r"C:\Users\a\AppData\Local\DiskLoom\Broker")
        ));
    }
}
