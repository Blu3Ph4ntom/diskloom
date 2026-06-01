use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use diskloom_core::{FileGraph, FileGraphBuilder, FileGraphError, FileKind};
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
    pub directories: u64,
    pub files: u64,
}

#[derive(Debug)]
pub struct FallbackScanner;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("failed to read `{path}`: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Graph(#[from] FileGraphError),
}

impl FallbackScanner {
    pub fn scan(options: ScanOptions) -> Result<(FileGraph, ScanSummary), ScanError> {
        let root_metadata = metadata_for(&options.root, options.follow_symlinks)?;
        let root_kind = kind_for(&root_metadata);
        let root_size = logical_size(&root_metadata, root_kind);
        let root_allocated = allocated_size(&options.root, root_kind, root_size);
        let root_name = root_name(&options.root);

        let mut builder = FileGraphBuilder::new();
        let root = builder.add_entry(
            None,
            &root_name,
            root_kind,
            root_size,
            root_allocated,
            modified_unix(&root_metadata),
        )?;

        let mut summary = ScanSummary::default();
        summary.entries += 1;
        bump_kind(&mut summary, root_kind);

        let mut pending = Vec::new();
        if root_kind == FileKind::Directory {
            pending.push((options.root, root));
        }

        while let Some((dir_path, parent)) = pending.pop() {
            let Ok(read_dir) = fs::read_dir(&dir_path) else {
                summary.inaccessible += 1;
                continue;
            };

            for child in read_dir {
                let Ok(child) = child else {
                    summary.inaccessible += 1;
                    continue;
                };
                let path = child.path();
                let Ok(metadata) = metadata_for(&path, options.follow_symlinks) else {
                    summary.inaccessible += 1;
                    continue;
                };

                let kind = kind_for(&metadata);
                let size = logical_size(&metadata, kind);
                let allocated = allocated_size(&path, kind, size);
                let name = child.file_name().to_string_lossy().into_owned();
                let id = builder.add_entry(
                    Some(parent),
                    &name,
                    kind,
                    size,
                    allocated,
                    modified_unix(&metadata),
                )?;

                summary.entries += 1;
                bump_kind(&mut summary, kind);

                if kind == FileKind::Directory {
                    pending.push((path, id));
                }
            }
        }

        Ok((builder.finish(), summary))
    }
}

fn metadata_for(path: &Path, follow_symlinks: bool) -> Result<fs::Metadata, ScanError> {
    let result = if follow_symlinks {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };

    result.map_err(|source| ScanError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn kind_for(metadata: &fs::Metadata) -> FileKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else if file_type.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    }
}

fn logical_size(metadata: &fs::Metadata, kind: FileKind) -> u64 {
    match kind {
        FileKind::File => metadata.len(),
        FileKind::Directory | FileKind::Symlink | FileKind::Other => 0,
    }
}

#[cfg(windows)]
fn allocated_size(path: &Path, kind: FileKind, fallback: u64) -> u64 {
    use std::os::windows::ffi::OsStrExt;

    use windows::{Win32::Storage::FileSystem::GetCompressedFileSizeW, core::PCWSTR};

    if kind != FileKind::File {
        return 0;
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0_u32;
    // SAFETY: The path buffer is null-terminated and remains valid for the call.
    let low = unsafe { GetCompressedFileSizeW(PCWSTR(wide.as_ptr()), Some(&mut high)) };
    if low == u32::MAX && high == 0 {
        return fallback;
    }

    ((high as u64) << 32) | u64::from(low)
}

#[cfg(not(windows))]
fn allocated_size(_: &Path, kind: FileKind, fallback: u64) -> u64 {
    if kind == FileKind::File { fallback } else { 0 }
}

fn modified_unix(metadata: &fs::Metadata) -> i64 {
    metadata.modified().map_or(0, system_time_to_unix)
}

fn system_time_to_unix(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(error) => -(error.duration().as_secs() as i64),
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

fn bump_kind(summary: &mut ScanSummary, kind: FileKind) {
    match kind {
        FileKind::Directory => summary.directories += 1,
        FileKind::File => summary.files += 1,
        FileKind::Symlink | FileKind::Other => {}
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{FallbackScanner, ScanOptions};

    #[test]
    fn scan_should_build_graph_for_directory_tree() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src").join("main.rs"), b"fn main() {}").unwrap();
        fs::write(temp.path().join("readme.md"), b"DiskLoom").unwrap();

        let (graph, summary) = FallbackScanner::scan(ScanOptions::new(temp.path())).unwrap();

        assert_eq!(summary.files, 2);
        assert_eq!(summary.directories, 2);
        assert_eq!(graph.len(), 4);
    }

    #[test]
    fn scan_should_aggregate_root_size() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("a.bin"), [0_u8; 8]).unwrap();
        fs::write(temp.path().join("b.bin"), [0_u8; 4]).unwrap();

        let (graph, _) = FallbackScanner::scan(ScanOptions::new(temp.path())).unwrap();
        let root = graph.ids().next().unwrap();
        let stats = graph.stats(root).unwrap();

        assert_eq!(stats.total_size.bytes(), 12);
    }
}
