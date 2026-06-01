use std::collections::BTreeMap;

use diskloom_core::{EntryId, FileGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub size: u64,
    pub name: String,
    pub modified_unix: i64,
    pub entries: Vec<EntryId>,
}

#[must_use]
pub fn find_duplicate_candidates(graph: &FileGraph) -> Vec<DuplicateCandidate> {
    let mut groups: BTreeMap<(u64, String, i64), Vec<EntryId>> = BTreeMap::new();

    for id in graph.ids() {
        let Some(stats) = graph.stats(id) else {
            continue;
        };
        let Some(entry) = graph.entry(id) else {
            continue;
        };
        if entry.flags.contains(diskloom_core::EntryFlags::DIRECTORY) {
            continue;
        }
        if stats.own_size.bytes() == 0 {
            continue;
        }
        let Some(name) = graph.name(id) else {
            continue;
        };

        groups
            .entry((
                stats.own_size.bytes(),
                name.to_lowercase(),
                entry.modified_unix,
            ))
            .or_default()
            .push(id);
    }

    groups
        .into_iter()
        .filter_map(|((size, name, modified_unix), entries)| {
            (entries.len() > 1).then_some(DuplicateCandidate {
                size,
                name,
                modified_unix,
                entries,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use diskloom_core::{FileGraphBuilder, FileKind};

    use super::find_duplicate_candidates;

    #[test]
    fn find_duplicate_candidates_should_group_by_size_name_and_modified_date() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "copy.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "COPY.bin", FileKind::File, 10, 10, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "other.bin", FileKind::File, 10, 10, 100)
            .unwrap();

        let graph = builder.finish();
        let candidates = find_duplicate_candidates(&graph);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].entries.len(), 2);
    }
}
