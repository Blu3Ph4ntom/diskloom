use std::{fs, io, path::Path, process::Command};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShellActionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Windows shell action is only supported on Windows")]
    UnsupportedPlatform,
    #[cfg(windows)]
    #[error("ShellExecuteW failed with code {0}")]
    ShellExecuteFailed(isize),
    #[cfg(windows)]
    #[error("SHFileOperationW failed with code {0}")]
    FileOperationFailed(i32),
}

pub fn open_in_explorer(path: impl AsRef<Path>) -> Result<(), ShellActionError> {
    let path = path.as_ref();
    let mut command = Command::new("explorer");
    if path.is_dir() {
        command.arg(path);
    } else {
        command.arg(format!("/select,{}", path.display()));
    }
    command.spawn()?;
    Ok(())
}

pub fn rename_path(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<(), ShellActionError> {
    fs::rename(from, to)?;
    Ok(())
}

#[cfg(windows)]
pub fn show_properties(path: impl AsRef<Path>) -> Result<(), ShellActionError> {
    use windows::{
        Win32::{
            Foundation::HWND,
            UI::{
                Shell::{
                    SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
                },
                WindowsAndMessaging::SW_SHOW,
            },
        },
        core::PCWSTR,
    };

    let verb = to_wide("properties");
    let file = to_wide_path(path.as_ref());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC,
        hwnd: HWND::default(),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOW.0,
        ..Default::default()
    };

    // SAFETY: The verb and file buffers are null-terminated and live for the call.
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| ShellActionError::ShellExecuteFailed(error.code().0 as isize))?;

    Ok(())
}

#[cfg(not(windows))]
pub fn show_properties(_: impl AsRef<Path>) -> Result<(), ShellActionError> {
    Err(ShellActionError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn recycle_delete(path: impl AsRef<Path>) -> Result<(), ShellActionError> {
    use windows::{
        Win32::{
            Foundation::HWND,
            UI::Shell::{
                FO_DELETE, FOF_ALLOWUNDO, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW,
                SHFileOperationW,
            },
        },
        core::{BOOL, PCWSTR},
    };

    let from = double_null_terminated_path(path.as_ref());
    let mut operation = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
        fAnyOperationsAborted: BOOL::from(false),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };

    // SAFETY: SHFILEOPSTRUCTW points to buffers that live for the duration of the call. `pFrom`
    // is double-null-terminated as required by SHFileOperationW.
    let code = unsafe { SHFileOperationW(&mut operation) };
    if code != 0 {
        return Err(ShellActionError::FileOperationFailed(code));
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn recycle_delete(_: impl AsRef<Path>) -> Result<(), ShellActionError> {
    Err(ShellActionError::UnsupportedPlatform)
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn to_wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(windows)]
fn double_null_terminated_path(path: &Path) -> Vec<u16> {
    let mut wide = to_wide_path(path);
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::rename_path;

    #[test]
    fn rename_path_should_rename_file() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from.txt");
        let to = temp.path().join("to.txt");
        std::fs::write(&from, b"diskloom").unwrap();

        rename_path(&from, &to).unwrap();

        assert!(to.exists());
    }
}
