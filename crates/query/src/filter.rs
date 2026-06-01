use std::{cmp::Ordering, collections::BinaryHeap, path::Path};

use diskloom_core::{EntryFlags, EntryId, FileGraph};
use regex::{Regex, RegexBuilder};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Allocated,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Debug, Clone)]
pub enum NameMatcher {
    Contains {
        needle: String,
        case_sensitive: bool,
    },
    Regex(Regex),
}

#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    pub name: Option<NameMatcher>,
    pub extension: Option<String>,
    pub path: Option<NameMatcher>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub min_allocated: Option<u64>,
    pub max_allocated: Option<u64>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    pub include_directories: bool,
}

#[derive(Debug, Clone)]
pub struct CompiledFilter {
    filter: QueryFilter,
}

#[derive(Debug, Error)]
pub enum FilterError {
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),
}

impl QueryFilter {
    pub fn compile(self) -> Result<CompiledFilter, FilterError> {
        Ok(CompiledFilter { filter: self })
    }
}

impl NameMatcher {
    #[must_use]
    pub fn contains(needle: impl Into<String>) -> Self {
        Self::Contains {
            needle: needle.into(),
            case_sensitive: false,
        }
    }

    #[must_use]
    pub fn contains_case_sensitive(needle: impl Into<String>) -> Self {
        Self::Contains {
            needle: needle.into(),
            case_sensitive: true,
        }
    }

    pub fn regex(pattern: &str) -> Result<Self, FilterError> {
        Ok(Self::Regex(
            RegexBuilder::new(pattern).case_insensitive(true).build()?,
        ))
    }

    #[must_use]
    pub fn matches(&self, value: &str) -> bool {
        match self {
            Self::Contains {
                needle,
                case_sensitive,
            } => {
                if *case_sensitive {
                    value.contains(needle)
                } else {
                    value.to_lowercase().contains(&needle.to_lowercase())
                }
            }
            Self::Regex(regex) => regex.is_match(value),
        }
    }
}

impl CompiledFilter {
    #[must_use]
    pub fn matches(&self, graph: &FileGraph, id: EntryId) -> bool {
        let Some(entry) = graph.entry(id) else {
            return false;
        };
        let Some(stats) = graph.stats(id) else {
            return false;
        };

        let is_directory = entry.flags.contains(EntryFlags::DIRECTORY);
        if is_directory && !self.filter.include_directories {
            return false;
        }

        if let Some(min_size) = self.filter.min_size
            && stats.total_size.bytes() < min_size
        {
            return false;
        }

        if let Some(max_size) = self.filter.max_size
            && stats.total_size.bytes() > max_size
        {
            return false;
        }

        if let Some(min_allocated) = self.filter.min_allocated
            && stats.total_allocated.bytes() < min_allocated
        {
            return false;
        }

        if let Some(max_allocated) = self.filter.max_allocated
            && stats.total_allocated.bytes() > max_allocated
        {
            return false;
        }

        if let Some(modified_after) = self.filter.modified_after
            && entry.modified_unix < modified_after
        {
            return false;
        }

        if let Some(modified_before) = self.filter.modified_before
            && entry.modified_unix > modified_before
        {
            return false;
        }

        let Some(name) = graph.name(id) else {
            return false;
        };

        if let Some(matcher) = &self.filter.name
            && !matcher.matches(name)
        {
            return false;
        }

        if let Some(extension) = &self.filter.extension
            && !extension_matches(name, extension)
        {
            return false;
        }

        if let Some(matcher) = &self.filter.path {
            let Some(path) = graph.reconstruct_path(id) else {
                return false;
            };
            if !matcher.matches(&path.to_string_lossy()) {
                return false;
            }
        }

        true
    }

    pub fn matching_ids<'a>(&'a self, graph: &'a FileGraph) -> impl Iterator<Item = EntryId> + 'a {
        graph.ids().filter(|id| self.matches(graph, *id))
    }
}

pub fn sort_entries(graph: &FileGraph, ids: &mut [EntryId], key: SortKey, order: SortOrder) {
    ids.sort_by(|left, right| {
        let ordering = compare_entries(graph, *left, *right, key);
        match order {
            SortOrder::Ascending => ordering,
            SortOrder::Descending => ordering.reverse(),
        }
    });
}

pub fn top_entries_by_total_size<I>(graph: &FileGraph, ids: I, limit: usize) -> Vec<EntryId>
where
    I: IntoIterator<Item = EntryId>,
{
    top_entries_by_size(ids, limit, |id| {
        graph.stats(id).map_or(0, |stats| stats.total_size.bytes())
    })
}

pub fn top_entries_by_own_size<I>(graph: &FileGraph, ids: I, limit: usize) -> Vec<EntryId>
where
    I: IntoIterator<Item = EntryId>,
{
    top_entries_by_size(ids, limit, |id| {
        graph.stats(id).map_or(0, |stats| stats.own_size.bytes())
    })
}

fn top_entries_by_size<I>(
    ids: I,
    limit: usize,
    mut size_for: impl FnMut(EntryId) -> u64,
) -> Vec<EntryId>
where
    I: IntoIterator<Item = EntryId>,
{
    if limit == 0 {
        return Vec::new();
    }

    let mut heap = BinaryHeap::with_capacity(limit);
    for id in ids {
        let candidate = TopSizeCandidate {
            size: size_for(id),
            id,
        };
        if heap.len() < limit {
            heap.push(candidate);
        } else if heap
            .peek()
            .is_some_and(|worst| candidate.is_better_than(worst))
        {
            heap.pop();
            heap.push(candidate);
        }
    }

    let mut candidates = heap.into_vec();
    candidates.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    candidates
        .into_iter()
        .map(|candidate| candidate.id)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TopSizeCandidate {
    size: u64,
    id: EntryId,
}

impl TopSizeCandidate {
    fn is_better_than(self, other: &Self) -> bool {
        self.size > other.size || (self.size == other.size && self.id.0 < other.id.0)
    }
}

impl Ord for TopSizeCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .size
            .cmp(&self.size)
            .then_with(|| self.id.0.cmp(&other.id.0))
    }
}

impl PartialOrd for TopSizeCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_entries(graph: &FileGraph, left: EntryId, right: EntryId, key: SortKey) -> Ordering {
    match key {
        SortKey::Name => graph.name(left).cmp(&graph.name(right)),
        SortKey::Size => graph
            .stats(left)
            .map(|stats| stats.total_size.bytes())
            .cmp(&graph.stats(right).map(|stats| stats.total_size.bytes())),
        SortKey::Allocated => graph
            .stats(left)
            .map(|stats| stats.total_allocated.bytes())
            .cmp(
                &graph
                    .stats(right)
                    .map(|stats| stats.total_allocated.bytes()),
            ),
        SortKey::Modified => graph
            .entry(left)
            .map(|entry| entry.modified_unix)
            .cmp(&graph.entry(right).map(|entry| entry.modified_unix)),
    }
}

fn extension_matches(name: &str, expected: &str) -> bool {
    let expected = expected.trim_start_matches('.').to_lowercase();
    Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase() == expected)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use diskloom_core::{FileGraph, FileGraphBuilder, FileKind};

    use super::{
        NameMatcher, QueryFilter, SortKey, SortOrder, sort_entries, top_entries_by_own_size,
        top_entries_by_total_size,
    };

    fn sample_graph() -> FileGraph {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 100)
            .unwrap();
        builder
            .add_entry(Some(root), "app.exe", FileKind::File, 50, 64, 200)
            .unwrap();
        builder
            .add_entry(Some(root), "notes.txt", FileKind::File, 10, 16, 300)
            .unwrap();
        builder.finish()
    }

    #[test]
    fn filter_should_match_name_and_extension() {
        let graph = sample_graph();
        let filter = QueryFilter {
            name: Some(NameMatcher::contains("NOTES")),
            extension: Some("txt".to_owned()),
            include_directories: false,
            ..QueryFilter::default()
        }
        .compile()
        .unwrap();

        let matches: Vec<_> = filter.matching_ids(&graph).collect();

        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn filter_should_match_full_path_lazily() {
        let graph = sample_graph();
        let filter = QueryFilter {
            path: Some(NameMatcher::contains("root")),
            include_directories: true,
            ..QueryFilter::default()
        }
        .compile()
        .unwrap();

        let matches: Vec<_> = filter.matching_ids(&graph).collect();

        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn sort_entries_should_order_by_size_descending() {
        let graph = sample_graph();
        let mut ids: Vec<_> = graph.ids().collect();

        sort_entries(&graph, &mut ids, SortKey::Size, SortOrder::Descending);

        assert_eq!(graph.name(ids[0]), Some("root"));
    }

    #[test]
    fn top_entries_by_total_size_should_keep_largest_entries() {
        let graph = sample_graph();

        let ids = top_entries_by_total_size(&graph, graph.ids(), 2);

        assert_eq!(ids.len(), 2);
        assert_eq!(graph.name(ids[0]), Some("root"));
        assert_eq!(graph.name(ids[1]), Some("app.exe"));
    }

    #[test]
    fn top_entries_by_own_size_should_not_promote_directory_totals() {
        let graph = sample_graph();

        let ids = top_entries_by_own_size(&graph, graph.ids(), 1);

        assert_eq!(graph.name(ids[0]), Some("app.exe"));
    }

    #[test]
    fn top_entries_by_total_size_should_respect_zero_limit() {
        let graph = sample_graph();

        let ids = top_entries_by_total_size(&graph, graph.ids(), 0);

        assert!(ids.is_empty());
    }
}
