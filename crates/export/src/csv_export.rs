use std::io::Write;

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

    for id in graph.ids() {
        write_entry(graph, &mut writer, options, id)?;
    }

    writer.flush()?;
    Ok(())
}

fn write_entry<W: Write>(
    graph: &FileGraph,
    writer: &mut csv::Writer<W>,
    options: CsvExportOptions,
    id: EntryId,
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
    let path = graph
        .reconstruct_path(id)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let kind = if is_directory { "directory" } else { "file" };

    writer.write_record([
        path,
        name.to_owned(),
        kind.to_owned(),
        stats.own_size.bytes().to_string(),
        stats.own_allocated.bytes().to_string(),
        stats.total_size.bytes().to_string(),
        stats.total_allocated.bytes().to_string(),
        entry.modified_unix.to_string(),
    ])?;

    Ok(())
}
