use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "diskloom-setup",
    about = "Install DiskLoom for Windows.",
    disable_version_flag = true
)]
struct Args {
    /// Folder containing diskloom.exe and dlm.exe.
    #[arg(long, value_name = "DIR")]
    source: Option<PathBuf>,

    /// Installation directory.
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,

    /// Do not add the install directory to the machine PATH.
    #[arg(long)]
    no_path: bool,

    /// Do not create a Start Menu shortcut.
    #[arg(long)]
    no_shortcut: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("DiskLoom setup failed:\n{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();

    ensure_elevated()?;

    let source_dir = match args.source {
        Some(source) => source,
        None => current_exe_dir()?,
    };
    let install_dir = match args.install_dir {
        Some(install_dir) => install_dir,
        None => default_install_dir(),
    };

    install_bundle(&source_dir, &install_dir, !args.no_path, !args.no_shortcut)?;

    println!("DiskLoom installed to {}", install_dir.display());
    if !args.no_path {
        println!("dlm.exe installed and the machine PATH was updated.");
    }
    if !args.no_shortcut {
        println!("Start Menu shortcut created.");
    }

    Ok(())
}

#[cfg(windows)]
fn ensure_elevated() -> Result<()> {
    if diskloom_windows::is_process_elevated()? {
        return Ok(());
    }

    diskloom_windows::relaunch_current_process_elevated(env::args_os().skip(1))?;
    println!("DiskLoom setup requested administrator access.");
    std::process::exit(0);
}

#[cfg(not(windows))]
fn ensure_elevated() -> Result<()> {
    bail!("DiskLoom setup is Windows-only");
}

fn current_exe_dir() -> Result<PathBuf> {
    let exe = env::current_exe().context("failed to locate setup executable")?;
    exe.parent()
        .map(Path::to_path_buf)
        .context("setup executable has no parent directory")
}

fn default_install_dir() -> PathBuf {
    env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("DiskLoom")
}

fn install_bundle(
    source_dir: &Path,
    install_dir: &Path,
    add_path: bool,
    add_shortcut: bool,
) -> Result<()> {
    let source_dir = source_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve source directory {}",
            source_dir.display()
        )
    })?;
    let install_dir = absolutize(install_dir)?;
    let gui_source = source_dir.join("diskloom.exe");
    let cli_source = source_dir.join("dlm.exe");

    ensure_file(&gui_source)?;
    ensure_file(&cli_source)?;

    fs::create_dir_all(&install_dir)
        .with_context(|| format!("failed to create {}", install_dir.display()))?;

    copy_file(&gui_source, &install_dir.join("diskloom.exe"))?;
    copy_file(&cli_source, &install_dir.join("dlm.exe"))?;
    for name in ["README.md", "LICENSE-MIT", "LICENSE-APACHE"] {
        copy_optional_file(&source_dir.join(name), &install_dir.join(name))?;
    }
    copy_docs(
        &source_dir.join("docs"),
        &install_dir.join("docs"),
        &install_dir,
    )?;

    if add_path {
        let changed = add_to_machine_path(&install_dir)?;
        if changed {
            broadcast_environment_change();
        }
    }

    if add_shortcut {
        create_start_menu_shortcut(&install_dir)?;
    }

    Ok(())
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Ok(env::current_dir()
        .context("failed to resolve current directory")?
        .join(path))
}

fn ensure_file(path: &Path) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("missing required file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a file", path.display());
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if paths_are_same_file(source, destination) {
        return Ok(());
    }
    fs::copy(source, destination).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn copy_optional_file(source: &Path, destination: &Path) -> Result<()> {
    if source.exists() {
        copy_file(source, destination)?;
    }
    Ok(())
}

fn paths_are_same_file(source: &Path, destination: &Path) -> bool {
    let Ok(source) = source.canonicalize() else {
        return false;
    };
    let Ok(destination) = destination.canonicalize() else {
        return false;
    };
    source == destination
}

fn copy_docs(source_docs: &Path, destination_docs: &Path, install_dir: &Path) -> Result<()> {
    if !source_docs.exists() {
        return Ok(());
    }

    if destination_docs.exists() {
        remove_existing_docs(destination_docs, install_dir)?;
    }
    copy_dir_recursive(source_docs, destination_docs)
}

fn remove_existing_docs(destination_docs: &Path, install_dir: &Path) -> Result<()> {
    let destination_docs = destination_docs
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", destination_docs.display()))?;
    let install_dir = install_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", install_dir.display()))?;
    if !destination_docs.starts_with(&install_dir) {
        bail!(
            "refusing to remove docs outside install directory: {}",
            destination_docs.display()
        );
    }

    fs::remove_dir_all(&destination_docs)
        .with_context(|| format!("failed to replace {}", destination_docs.display()))?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", source.display()))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", source_path.display()))?;
        if metadata.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            copy_file(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn add_to_machine_path(install_dir: &Path) -> Result<bool> {
    let install_path = normalized_path_text(install_dir);
    let current = read_machine_path()?;
    if path_value_contains(&current, &install_path) {
        return Ok(false);
    }

    let updated = append_path_value(&current, &install_path);
    write_machine_path(&updated)?;
    Ok(true)
}

#[cfg(not(windows))]
fn add_to_machine_path(_: &Path) -> Result<bool> {
    bail!("machine PATH install is Windows-only")
}

fn normalized_path_text(path: &Path) -> String {
    trim_trailing_separators(&path.to_string_lossy()).to_owned()
}

fn trim_trailing_separators(path: &str) -> &str {
    let trimmed = path.trim();
    let mut end = trimmed.len();
    while end > 0 {
        let Some(previous) = trimmed[..end].chars().next_back() else {
            break;
        };
        if previous != '\\' && previous != '/' {
            break;
        }
        end -= previous.len_utf8();
    }
    &trimmed[..end]
}

fn path_value_contains(path_value: &str, entry: &str) -> bool {
    let expected = trim_trailing_separators(entry);
    path_value.split(';').any(|part| {
        let part = trim_trailing_separators(part);
        !part.is_empty() && part.eq_ignore_ascii_case(expected)
    })
}

fn append_path_value(path_value: &str, entry: &str) -> String {
    let current = path_value.trim_end_matches(';');
    if current.is_empty() {
        entry.to_owned()
    } else {
        format!("{current};{entry}")
    }
}

#[cfg(windows)]
fn read_machine_path() -> Result<String> {
    use windows::{
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
            System::Registry::{
                HKEY_LOCAL_MACHINE, REG_VALUE_TYPE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
                RegGetValueW,
            },
        },
        core::PCWSTR,
    };

    let subkey = wide(REGISTRY_ENVIRONMENT_KEY);
    let value = wide("Path");
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut value_type = REG_VALUE_TYPE::default();
    let mut byte_len = 0_u32;

    // SAFETY: Registry key/value strings are null-terminated and output pointers are valid.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            None,
            Some(&mut byte_len),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(String::new());
    }
    if status != ERROR_SUCCESS {
        bail!(
            "failed to read machine PATH size: Windows error {}",
            status.0
        );
    }
    if byte_len == 0 {
        return Ok(String::new());
    }

    let mut buffer = vec![0_u16; byte_len.div_ceil(2) as usize];
    // SAFETY: `buffer` is writable storage for the byte count reported by RegGetValueW.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            Some(&mut value_type),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut byte_len),
        )
    };
    if status != ERROR_SUCCESS {
        bail!("failed to read machine PATH: Windows error {}", status.0);
    }

    let len = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

#[cfg(windows)]
fn write_machine_path(path_value: &str) -> Result<()> {
    use windows::{
        Win32::{
            Foundation::ERROR_SUCCESS,
            System::Registry::{HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, RegSetKeyValueW},
        },
        core::PCWSTR,
    };

    let subkey = wide(REGISTRY_ENVIRONMENT_KEY);
    let value = wide("Path");
    let data = wide(path_value);
    let byte_len = u32::try_from(data.len() * std::mem::size_of::<u16>())
        .context("machine PATH is too large to write")?;

    // SAFETY: Registry key/value/data strings are null-terminated and the data length matches
    // the UTF-16 buffer byte length.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            REG_EXPAND_SZ.0,
            Some(data.as_ptr().cast()),
            byte_len,
        )
    };
    if status != ERROR_SUCCESS {
        bail!("failed to write machine PATH: Windows error {}", status.0);
    }
    Ok(())
}

#[cfg(windows)]
fn broadcast_environment_change() {
    use windows::{
        Win32::{
            Foundation::{LPARAM, WPARAM},
            UI::WindowsAndMessaging::{
                HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
            },
        },
        core::PCWSTR,
    };

    let environment = wide("Environment");
    // SAFETY: The string pointer is valid for the duration of the synchronous message call.
    let _ = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(PCWSTR(environment.as_ptr()).0 as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        )
    };
}

#[cfg(not(windows))]
fn broadcast_environment_change() {}

#[cfg(windows)]
fn create_start_menu_shortcut(install_dir: &Path) -> Result<()> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IPersistFile,
            },
            UI::Shell::{IShellLinkW, ShellLink},
        },
        core::{Interface, PCWSTR},
    };

    let programs_dir = env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(r"Microsoft\Windows\Start Menu\Programs");
    fs::create_dir_all(&programs_dir)
        .with_context(|| format!("failed to create {}", programs_dir.display()))?;

    let shortcut_path = programs_dir.join("DiskLoom.lnk");
    let target = install_dir.join("diskloom.exe");
    let target_wide = wide_path(&target);
    let working_dir_wide = wide_path(install_dir);
    let description = wide("DiskLoom");
    let shortcut_wide = wide_path(&shortcut_path);

    // SAFETY: COM is initialized for this thread, and all PCWSTR buffers are null-terminated
    // and live until the shortcut has been saved.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(target_wide.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(working_dir_wide.as_ptr()))?;
        link.SetDescription(PCWSTR(description.as_ptr()))?;
        link.SetIconLocation(PCWSTR(target_wide.as_ptr()), 0)?;
        let persist: IPersistFile = link.cast()?;
        persist.Save(PCWSTR(shortcut_wide.as_ptr()), true)?;
        CoUninitialize();
    }

    Ok(())
}

#[cfg(not(windows))]
fn create_start_menu_shortcut(_: &Path) -> Result<()> {
    bail!("Start Menu shortcut install is Windows-only")
}

#[cfg(windows)]
const REGISTRY_ENVIRONMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{append_path_value, path_value_contains, trim_trailing_separators};

    #[test]
    fn trim_trailing_separators_should_keep_drive_root_name() {
        assert_eq!(
            trim_trailing_separators(r"C:\Program Files\DiskLoom\"),
            r"C:\Program Files\DiskLoom"
        );
    }

    #[test]
    fn path_value_contains_should_match_case_insensitive_paths() {
        assert!(path_value_contains(
            r"C:\Windows;C:\Program Files\DiskLoom",
            r"c:\program files\diskloom\"
        ));
    }

    #[test]
    fn path_value_contains_should_not_match_partial_paths() {
        assert!(!path_value_contains(
            r"C:\Program Files\DiskLoomer",
            r"C:\Program Files\DiskLoom"
        ));
    }

    #[test]
    fn append_path_value_should_preserve_existing_entries() {
        assert_eq!(
            append_path_value(r"C:\Windows;", r"C:\Program Files\DiskLoom"),
            r"C:\Windows;C:\Program Files\DiskLoom"
        );
    }
}
