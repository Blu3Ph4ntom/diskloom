use std::path::PathBuf;

use thiserror::Error;

use crate::{ByteSize, StringId, StringInterner, StringTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
    Other,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EntryFlags: u16 {
        const DIRECTORY = 1 << 0;
        const SYMLINK = 1 << 1;
        const HARD_LINK = 1 << 2;
        const INACCESSIBLE = 1 << 3;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeStats {
    pub own_size: ByteSize,
    pub own_allocated: ByteSize,
    pub total_size: ByteSize,
    pub total_allocated: ByteSize,
    pub descendants: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphEntry {
    pub id: EntryId,
    pub parent: Option<EntryId>,
    pub name: StringId,
    pub flags: EntryFlags,
    pub modified_unix: i64,
}

#[derive(Debug, Error)]
pub enum FileGraphError {
    #[error("entry id {0} is out of bounds")]
    InvalidEntry(u32),
    #[error("entry {child:?} cannot use itself as parent")]
    SelfParent { child: EntryId },
}

#[derive(Debug, Clone)]
pub struct FileGraph {
    names: StringTable,
    parents: Vec<Option<EntryId>>,
    name_ids: Vec<StringId>,
    flags: Vec<EntryFlags>,
    modified_unix: Vec<i64>,
    own_size: Vec<u64>,
    own_allocated: Vec<u64>,
    total_size: Vec<u64>,
    total_allocated: Vec<u64>,
    descendants: Vec<u32>,
}

impl FileGraph {
    #[must_use]
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    #[must_use]
    pub fn entry(&self, id: EntryId) -> Option<GraphEntry> {
        let idx = id.0 as usize;
        Some(GraphEntry {
            id,
            parent: *self.parents.get(idx)?,
            name: *self.name_ids.get(idx)?,
            flags: *self.flags.get(idx)?,
            modified_unix: *self.modified_unix.get(idx)?,
        })
    }

    #[must_use]
    pub fn name(&self, id: EntryId) -> Option<&str> {
        let name_id = *self.name_ids.get(id.0 as usize)?;
        self.names.get(name_id)
    }

    #[must_use]
    pub fn stats(&self, id: EntryId) -> Option<NodeStats> {
        let idx = id.0 as usize;
        Some(NodeStats {
            own_size: ByteSize(*self.own_size.get(idx)?),
            own_allocated: ByteSize(*self.own_allocated.get(idx)?),
            total_size: ByteSize(*self.total_size.get(idx)?),
            total_allocated: ByteSize(*self.total_allocated.get(idx)?),
            descendants: *self.descendants.get(idx)?,
        })
    }

    pub fn ids(&self) -> impl Iterator<Item = EntryId> + '_ {
        (0..self.len()).map(|idx| EntryId(idx as u32))
    }

    pub fn children_of(&self, parent: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        self.parents
            .iter()
            .enumerate()
            .filter(move |(_, candidate)| **candidate == Some(parent))
            .map(|(idx, _)| EntryId(idx as u32))
    }

    #[must_use]
    pub fn reconstruct_path(&self, id: EntryId) -> Option<PathBuf> {
        let mut names = Vec::new();
        let mut current = Some(id);

        while let Some(entry_id) = current {
            names.push(self.name(entry_id)?);
            current = self.parents.get(entry_id.0 as usize).copied().flatten();
        }

        let mut path = PathBuf::new();
        for name in names.iter().rev() {
            path.push(name);
        }
        Some(path)
    }

    fn aggregate(&mut self) {
        self.total_size.clone_from(&self.own_size);
        self.total_allocated.clone_from(&self.own_allocated);
        self.descendants.fill(0);

        for idx in (0..self.parents.len()).rev() {
            let Some(parent) = self.parents[idx] else {
                continue;
            };
            let parent_idx = parent.0 as usize;
            self.total_size[parent_idx] =
                self.total_size[parent_idx].saturating_add(self.total_size[idx]);
            self.total_allocated[parent_idx] =
                self.total_allocated[parent_idx].saturating_add(self.total_allocated[idx]);
            self.descendants[parent_idx] = self.descendants[parent_idx]
                .saturating_add(self.descendants[idx])
                .saturating_add(1);
        }
    }
}

#[derive(Debug, Default)]
pub struct FileGraphBuilder {
    names: StringInterner,
    parents: Vec<Option<EntryId>>,
    name_ids: Vec<StringId>,
    flags: Vec<EntryFlags>,
    modified_unix: Vec<i64>,
    own_size: Vec<u64>,
    own_allocated: Vec<u64>,
}

impl FileGraphBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_entry(
        &mut self,
        parent: Option<EntryId>,
        name: &str,
        kind: FileKind,
        size: u64,
        allocated: u64,
        modified_unix: i64,
    ) -> Result<EntryId, FileGraphError> {
        let id = EntryId(self.parents.len() as u32);
        if parent == Some(id) {
            return Err(FileGraphError::SelfParent { child: id });
        }

        if let Some(parent_id) = parent {
            self.parents
                .get(parent_id.0 as usize)
                .ok_or(FileGraphError::InvalidEntry(parent_id.0))?;
        }

        let mut flags = EntryFlags::empty();
        match kind {
            FileKind::Directory => flags.insert(EntryFlags::DIRECTORY),
            FileKind::Symlink => flags.insert(EntryFlags::SYMLINK),
            FileKind::File | FileKind::Other => {}
        }

        let name_id = self.names.intern(name);
        self.parents.push(parent);
        self.name_ids.push(name_id);
        self.flags.push(flags);
        self.modified_unix.push(modified_unix);
        self.own_size.push(size);
        self.own_allocated.push(allocated);

        Ok(id)
    }

    #[must_use]
    pub fn finish(self) -> FileGraph {
        let mut graph = FileGraph {
            names: self.names.finish(),
            parents: self.parents,
            name_ids: self.name_ids,
            flags: self.flags,
            modified_unix: self.modified_unix,
            own_size: self.own_size,
            own_allocated: self.own_allocated,
            total_size: Vec::new(),
            total_allocated: Vec::new(),
            descendants: Vec::new(),
        };
        graph.total_size.resize(graph.own_size.len(), 0);
        graph.total_allocated.resize(graph.own_allocated.len(), 0);
        graph.descendants.resize(graph.own_size.len(), 0);
        graph.aggregate();
        graph
    }
}

#[cfg(test)]
mod tests {
    use super::{FileGraphBuilder, FileKind};

    #[test]
    fn finish_should_aggregate_directory_totals() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "a.bin", FileKind::File, 10, 16, 0)
            .unwrap();
        builder
            .add_entry(Some(root), "b.bin", FileKind::File, 20, 32, 0)
            .unwrap();

        let graph = builder.finish();
        let stats = graph.stats(root).unwrap();

        assert_eq!(stats.total_size.bytes(), 30);
        assert_eq!(stats.total_allocated.bytes(), 48);
        assert_eq!(stats.descendants, 2);
    }

    #[test]
    fn reconstruct_path_should_build_path_from_parent_chain() {
        let mut builder = FileGraphBuilder::new();
        let root = builder
            .add_entry(None, "root", FileKind::Directory, 0, 0, 0)
            .unwrap();
        let child = builder
            .add_entry(Some(root), "file.txt", FileKind::File, 5, 8, 0)
            .unwrap();

        let graph = builder.finish();

        assert_eq!(
            graph.reconstruct_path(child).unwrap().to_string_lossy(),
            "root\\file.txt"
        );
    }
}
