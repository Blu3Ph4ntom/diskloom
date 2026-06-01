//! Windows volume discovery and shell integration.

pub mod shell;
pub mod volume;

pub use shell::{ShellActionError, open_in_explorer, recycle_delete, rename_path, show_properties};
pub use volume::{VolumeInfo, VolumeKind, WindowsVolumeError, discover_volumes};
