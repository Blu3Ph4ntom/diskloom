//! Windows volume discovery and shell integration.

pub mod volume;

pub use volume::{VolumeInfo, VolumeKind, WindowsVolumeError, discover_volumes};
