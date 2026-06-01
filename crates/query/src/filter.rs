use diskloom_core::{EntryId, FileGraph};
use regex::Regex;
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
    Contains(String),
    Regex(Regex),
}

#[derive(Debug, Clone, Default)]
pub struct QueryFilter {
    pub name: Option<NameMatcher>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
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

impl CompiledFilter {
    #[must_use]
    pub fn matches(&self, graph: &FileGraph, id: EntryId) -> bool {
        let Some(stats) = graph.stats(id) else {
            return false;
        };

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

        if let Some(matcher) = &self.filter.name {
            let Some(name) = graph.name(id) else {
                return false;
            };

            return match matcher {
                NameMatcher::Contains(needle) => {
                    name.to_lowercase().contains(&needle.to_lowercase())
                }
                NameMatcher::Regex(regex) => regex.is_match(name),
            };
        }

        true
    }

    #[must_use]
    pub fn matching_ids<'a>(&'a self, graph: &'a FileGraph) -> impl Iterator<Item = EntryId> + 'a {
        graph.ids().filter(|id| self.matches(graph, *id))
    }
}
