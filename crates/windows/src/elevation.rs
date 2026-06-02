use std::{ffi::OsStr, io};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ElevationError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("administrator elevation is only supported on Windows")]
    UnsupportedPlatform,
    #[cfg(windows)]
    #[error(transparent)]
    Windows(#[from] windows::core::Error),
    #[cfg(windows)]
    #[error("ShellExecuteW runas failed with code {0}")]
    ShellExecuteFailed(isize),
    #[cfg(windows)]
    #[error("waiting for elevated process failed with code {0}")]
    WaitFailed(u32),
}

#[cfg(windows)]
pub fn is_process_elevated() -> Result<bool, ElevationError> {
    use std::mem::size_of;

    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = HANDLE::default();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle for the current process, and
    // `OpenProcessToken` writes the token handle into `token` when it succeeds.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0_u32;
    // SAFETY: `elevation` is valid writable storage for TOKEN_ELEVATION, and `returned` is a
    // valid output pointer for the number of bytes written.
    unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )?;
    }

    // SAFETY: `token` is a real handle returned by OpenProcessToken and is owned here.
    let _ = unsafe { CloseHandle(token) };

    Ok(elevation.TokenIsElevated != 0)
}

#[cfg(not(windows))]
pub fn is_process_elevated() -> Result<bool, ElevationError> {
    Err(ElevationError::UnsupportedPlatform)
}

#[cfg(windows)]
pub fn relaunch_current_process_elevated<I, S>(args: I) -> Result<(), ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::os::windows::ffi::OsStrExt;

    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::PCWSTR,
    };

    let exe = std::env::current_exe()?;
    let working_dir = std::env::current_dir()?;
    let parameters = join_windows_args(args);
    let verb = to_wide("runas");
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let params = to_wide(&parameters);
    let directory: Vec<u16> = working_dir
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    // SAFETY: The verb, file, parameter, and directory buffers are null-terminated and live for
    // the duration of the ShellExecuteW call.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR(directory.as_ptr()),
            SW_SHOWNORMAL,
        )
    };

    let code = result.0 as isize;
    if code <= 32 {
        return Err(ElevationError::ShellExecuteFailed(code));
    }

    Ok(())
}

#[cfg(windows)]
pub fn run_current_process_elevated_and_wait<I, S>(args: I) -> Result<u32, ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_current_process_elevated_and_wait_with_show(
        args,
        windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
    )
}

#[cfg(windows)]
pub fn run_current_process_elevated_hidden_and_wait<I, S>(args: I) -> Result<u32, ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_current_process_elevated_and_wait_with_show(
        args,
        windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
    )
}

#[cfg(windows)]
fn run_current_process_elevated_and_wait_with_show<I, S>(
    args: I,
    show_command: i32,
) -> Result<u32, ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    use std::{mem::size_of, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
    };

    let exe = std::env::current_exe()?;
    let working_dir = std::env::current_dir()?;
    let parameters = join_windows_args(args);
    let verb = to_wide("runas");
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let params = to_wide(&parameters);
    let directory: Vec<u16> = working_dir
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();

    let mut execute_info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: show_command,
        ..SHELLEXECUTEINFOW::default()
    };

    // SAFETY: The SHELLEXECUTEINFOW structure points to null-terminated buffers that live for
    // the call, and SEE_MASK_NOCLOSEPROCESS asks Windows to return an owned process handle.
    if unsafe { ShellExecuteExW(&mut execute_info) } == 0 {
        return Err(ElevationError::Io(io::Error::last_os_error()));
    }

    // SAFETY: ShellExecuteExW succeeded and hProcess is owned by this process when
    // SEE_MASK_NOCLOSEPROCESS is set.
    let wait_result = unsafe { WaitForSingleObject(execute_info.hProcess, INFINITE) };
    if wait_result != WAIT_OBJECT_0 {
        // SAFETY: hProcess is owned by this process.
        let _ = unsafe { CloseHandle(execute_info.hProcess) };
        return Err(ElevationError::WaitFailed(wait_result));
    }

    let mut exit_code = 0_u32;
    // SAFETY: hProcess is valid until CloseHandle and exit_code points to writable storage.
    if unsafe { GetExitCodeProcess(execute_info.hProcess, &mut exit_code) } == 0 {
        // SAFETY: hProcess is owned by this process.
        let _ = unsafe { CloseHandle(execute_info.hProcess) };
        return Err(ElevationError::Io(io::Error::last_os_error()));
    }
    // SAFETY: hProcess is owned by this process.
    let _ = unsafe { CloseHandle(execute_info.hProcess) };

    Ok(exit_code)
}

#[cfg(not(windows))]
pub fn run_current_process_elevated_hidden_and_wait<I, S>(_: I) -> Result<u32, ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Err(ElevationError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn run_current_process_elevated_and_wait<I, S>(_: I) -> Result<u32, ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Err(ElevationError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn relaunch_current_process_elevated<I, S>(_: I) -> Result<(), ElevationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Err(ElevationError::UnsupportedPlatform)
}

fn join_windows_args<I, S>(args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .map(|arg| quote_windows_arg(&arg.as_ref().to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_windows_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_owned();
    }

    if !arg.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return arg.to_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0_usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                push_backslashes(&mut quoted, backslashes * 2 + 1);
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                push_backslashes(&mut quoted, backslashes);
                quoted.push(ch);
                backslashes = 0;
            }
        }
    }
    push_backslashes(&mut quoted, backslashes * 2);
    quoted.push('"');
    quoted
}

fn push_backslashes(output: &mut String, count: usize) {
    output.extend(std::iter::repeat_n('\\', count));
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{join_windows_args, quote_windows_arg};

    #[test]
    fn quote_windows_arg_should_leave_plain_arguments_unquoted() {
        assert_eq!(quote_windows_arg("scan"), "scan");
    }

    #[test]
    fn quote_windows_arg_should_quote_spaces() {
        assert_eq!(
            quote_windows_arg("C:\\Program Files"),
            "\"C:\\Program Files\""
        );
    }

    #[test]
    fn quote_windows_arg_should_escape_quotes_and_trailing_backslashes() {
        assert_eq!(quote_windows_arg("a\"b\\"), "\"a\\\"b\\\\\"");
    }

    #[test]
    fn join_windows_args_should_build_parameter_string() {
        let args = ["scan", "C:\\", "--name", "big file"];

        assert_eq!(
            join_windows_args(args),
            "scan C:\\ --name \"big file\"".to_owned()
        );
    }
}
