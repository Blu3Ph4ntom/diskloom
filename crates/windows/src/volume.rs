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
    #[cfg(windows)]
    #[error("{operation} failed: {source}")]
    Api {
        operation: &'static str,
        source: windows::core::Error,
    },
}

#[cfg(windows)]
pub fn discover_volumes() -> Result<Vec<VolumeInfo>, WindowsVolumeError> {
    let roots = logical_drive_roots()?;
    Ok(roots
        .into_iter()
        .map(|root| {
            let kind = volume_kind(&root).unwrap_or(VolumeKind::Unknown);
            VolumeInfo { root, kind }
        })
        .collect())
}

#[cfg(not(windows))]
pub fn discover_volumes() -> Result<Vec<VolumeInfo>, WindowsVolumeError> {
    Err(WindowsVolumeError::UnsupportedPlatform)
}

#[cfg(windows)]
fn logical_drive_roots() -> Result<Vec<String>, WindowsVolumeError> {
    use windows::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

    // SAFETY: Passing None asks Windows for the required UTF-16 buffer length.
    let needed = unsafe { GetLogicalDriveStringsW(None) };
    if needed == 0 {
        return Err(WindowsVolumeError::Api {
            operation: "GetLogicalDriveStringsW",
            source: windows::core::Error::from_win32(),
        });
    }

    let mut buffer = vec![0_u16; needed as usize + 1];
    // SAFETY: The mutable slice is valid for the duration of the call and Windows writes at most
    // the slice length supplied by the generated binding.
    let written = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) };
    if written == 0 {
        return Err(WindowsVolumeError::Api {
            operation: "GetLogicalDriveStringsW",
            source: windows::core::Error::from_win32(),
        });
    }

    Ok(parse_drive_strings(&buffer[..written as usize]))
}

#[cfg(windows)]
fn volume_kind(root: &str) -> Result<VolumeKind, WindowsVolumeError> {
    use windows::{Win32::Storage::FileSystem::GetVolumeInformationW, core::PCWSTR};

    let root_wide = to_wide(root);
    let mut fs_name = [0_u16; 32];

    // SAFETY: `root_wide` is null-terminated and `fs_name` is a valid writable UTF-16 buffer.
    unsafe {
        GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name),
        )
    }
    .map_err(|source| WindowsVolumeError::Api {
        operation: "GetVolumeInformationW",
        source,
    })?;

    let name = trim_nul_utf16(&fs_name);
    if name.eq_ignore_ascii_case("NTFS") {
        Ok(VolumeKind::Ntfs)
    } else if name.is_empty() {
        Ok(VolumeKind::Unknown)
    } else {
        Ok(VolumeKind::Other(name))
    }
}

#[cfg(windows)]
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn parse_drive_strings(buffer: &[u16]) -> Vec<String> {
    let mut roots = Vec::new();
    let mut start = 0;

    for (idx, code_unit) in buffer.iter().enumerate() {
        if *code_unit != 0 {
            continue;
        }
        if idx == start {
            break;
        }

        roots.push(String::from_utf16_lossy(&buffer[start..idx]));
        start = idx + 1;
    }

    roots
}

fn trim_nul_utf16(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|code_unit| *code_unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::{parse_drive_strings, trim_nul_utf16};

    #[test]
    fn parse_drive_strings_should_read_multi_string_buffer() {
        let input: Vec<u16> = "C:\\\0D:\\\0\0".encode_utf16().collect();

        let roots = parse_drive_strings(&input);

        assert_eq!(roots, ["C:\\", "D:\\"]);
    }

    #[test]
    fn trim_nul_utf16_should_stop_at_first_nul() {
        let input: Vec<u16> = "NTFS\0ignored".encode_utf16().collect();

        assert_eq!(trim_nul_utf16(&input), "NTFS");
    }
}
