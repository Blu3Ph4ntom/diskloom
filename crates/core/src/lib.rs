//! Compact file graph and aggregation primitives for DiskLoom.

pub mod graph;
pub mod intern;
pub mod size;

pub use graph::{
    EntryFlags, EntryId, FileGraph, FileGraphBuilder, FileGraphError, FileKind, GraphEntry,
    NodeStats,
};
pub use intern::{StringId, StringInterner, StringTable};
pub use size::ByteSize;
