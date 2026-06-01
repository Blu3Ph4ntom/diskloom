use std::{collections::BTreeMap, path::Path};

use diskloom_core::{EntryFlags, FileGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTypeStat {
    pub extension: String,
    pub files: u64,
    pub size: u64,
    pub allocated: u64,
}

#[must_use]
pub fn file_type_stats(graph: &FileGraph, limit: usize) -> Vec<FileTypeStat> {
    let mut stats: BTreeMap<String, FileTypeStat> = BTreeMap::new();

    for id in graph.ids() {
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if entry.flags.contains(EntryFlags::DIRECTORY) {
            continue;
        }

        let Some(name) = graph.name(id) else {
            continue;
        };
        let Some(node_stats) = graph.stats(id) else {
            continue;
        };

        let extension = extension_label(name);
        let stat = stats.entry(extension.clone()).or_insert(FileTypeStat {
            extension,
            files: 0,
            size: 0,
            allocated: 0,
        });
        stat.files += 1;
        stat.size = stat.size.saturating_add(node_stats.own_size.bytes());
        stat.allocated = stat
            .allocated
            .saturating_add(node_stats.own_allocated.bytes());
    }

    let mut stats = stats.into_values().collect::<Vec<_>>();
    stats.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| right.files.cmp(&left.files))
            .then_with(|| left.extension.cmp(&right.extension))
    });
    stats.truncate(limit);
    stats
}

fn extension_label(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map(|extension| extension.to_ascii_lowercase())
        .unwrap_or_else(|| "(none)".to_owned())
}

#[cfg(test)]
mod tests {
    use diskloom_core::{FileGraphBuilder, FileKind};

    use super::file_type_stats;

    #[test]
    fn file_type_stats_should_group_files_by_lowercase_extension() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "a.TXT", FileKind::File, 10, 16, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "b.txt", FileKind::File, 20, 24, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "noext", FileKind::File, 5, 8, 0)
            .unwrap();

        let graph = builder.finish();
        let stats = file_type_stats(&graph, 10);

        assert_eq!(stats[0].extension, "txt");
        assert_eq!(stats[0].files, 2);
        assert_eq!(stats[0].size, 30);
        assert_eq!(stats[1].extension, "(none)");
    }
}
