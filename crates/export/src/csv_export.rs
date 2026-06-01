use std::{io::Write, path::MAIN_SEPARATOR};

use diskloom_core::{EntryId, FileGraph};
use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct CsvExportOptions {
    pub include_directories: bool,
}

impl Default for CsvExportOptions {
    fn default() -> Self {
        Self {
            include_directories: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum CsvExportError {
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn export_csv<W: Write>(
    graph: &FileGraph,
    writer: W,
    options: CsvExportOptions,
) -> Result<(), CsvExportError> {
    let mut writer = csv::Writer::from_writer(writer);
    writer.write_record([
        "path",
        "name",
        "kind",
        "size",
        "allocated",
        "total_size",
        "total_allocated",
        "modified_unix",
    ])?;

    let mut path = String::new();
    let mut path_parts = Vec::new();
    for id in graph.ids() {
        write_entry(graph, &mut writer, options, id, &mut path, &mut path_parts)?;
    }

    writer.flush()?;
    Ok(())
}

fn write_entry<'graph, W: Write>(
    graph: &'graph FileGraph,
    writer: &mut csv::Writer<W>,
    options: CsvExportOptions,
    id: EntryId,
    path: &mut String,
    path_parts: &mut Vec<&'graph str>,
) -> Result<(), CsvExportError> {
    let Some(entry) = graph.entry(id) else {
        return Ok(());
    };

    let is_directory = entry.flags.contains(diskloom_core::EntryFlags::DIRECTORY);
    if is_directory && !options.include_directories {
        return Ok(());
    }

    let Some(stats) = graph.stats(id) else {
        return Ok(());
    };
    let name = graph.name(id).unwrap_or_default();
    let path = reconstruct_path_into(graph, id, path, path_parts).unwrap_or_default();
    let kind = if is_directory { "directory" } else { "file" };
    let mut own_size = itoa::Buffer::new();
    let mut own_allocated = itoa::Buffer::new();
    let mut total_size = itoa::Buffer::new();
    let mut total_allocated = itoa::Buffer::new();
    let mut modified_unix = itoa::Buffer::new();

    writer.write_record([
        path,
        name,
        kind,
        own_size.format(stats.own_size.bytes()),
        own_allocated.format(stats.own_allocated.bytes()),
        total_size.format(stats.total_size.bytes()),
        total_allocated.format(stats.total_allocated.bytes()),
        modified_unix.format(entry.modified_unix),
    ])?;

    Ok(())
}

fn reconstruct_path_into<'graph, 'path>(
    graph: &'graph FileGraph,
    id: EntryId,
    path: &'path mut String,
    path_parts: &mut Vec<&'graph str>,
) -> Option<&'path str> {
    path.clear();
    path_parts.clear();

    let mut current = Some(id);
    while let Some(entry_id) = current {
        path_parts.push(graph.name(entry_id)?);
        current = graph.entry(entry_id)?.parent;
    }

    for part in path_parts.iter().rev() {
        if !path.is_empty() {
            path.push(MAIN_SEPARATOR);
        }
        path.push_str(part);
    }

    Some(path.as_str())
}

#[cfg(test)]
mod tests {
    use std::path::MAIN_SEPARATOR;

    use diskloom_core::{FileGraphBuilder, FileKind};

    use super::{CsvExportOptions, export_csv};

    #[test]
    fn export_csv_should_escape_paths_and_names() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "a,b.txt", FileKind::File, 5, 8, 0)
            .unwrap();
        let graph = builder.finish();

        let mut output = Vec::new();
        export_csv(&graph, &mut output, CsvExportOptions::default()).unwrap();
        let csv = String::from_utf8(output).unwrap();

        assert!(csv.contains("\"a,b.txt\""));
    }

    #[test]
    fn export_csv_should_skip_directories_when_requested() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "file.txt", FileKind::File, 5, 8, 0)
            .unwrap();
        let graph = builder.finish();

        let mut output = Vec::new();
        export_csv(
            &graph,
            &mut output,
            CsvExportOptions {
                include_directories: false,
            },
        )
        .unwrap();
        let csv = String::from_utf8(output).unwrap();

        assert!(!csv.contains(",directory,"));
    }

    #[test]
    fn export_csv_should_write_full_paths() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let src = builder
            .add_entry(Some(root), "src", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(src), "main.rs", FileKind::File, 5, 8, 0)
            .unwrap();
        let graph = builder.finish();

        let mut output = Vec::new();
        export_csv(&graph, &mut output, CsvExportOptions::default()).unwrap();
        let csv = String::from_utf8(output).unwrap();
        let path = format!("root{MAIN_SEPARATOR}src{MAIN_SEPARATOR}main.rs");

        assert!(csv.contains(&path));
    }
}
