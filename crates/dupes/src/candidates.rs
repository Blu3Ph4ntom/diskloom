use std::collections::BTreeMap;

use diskloom_core::{EntryId, FileGraph};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub size: u64,
    pub entries: Vec<EntryId>,
}

#[must_use]
pub fn find_duplicate_candidates(graph: &FileGraph) -> Vec<DuplicateCandidate> {
    let mut by_size: BTreeMap<u64, Vec<EntryId>> = BTreeMap::new();

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
        by_size.entry(stats.own_size.bytes()).or_default().push(id);
    }

    by_size
        .into_iter()
        .filter_map(|(size, entries)| {
            (entries.len() > 1).then_some(DuplicateCandidate { size, entries })
        })
        .collect()
}
