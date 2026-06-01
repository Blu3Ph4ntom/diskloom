//! Windows volume discovery and shell integration.

pub mod elevation;
pub mod shell;
pub mod volume;

pub use elevation::{ElevationError, is_process_elevated, relaunch_current_process_elevated};
pub use shell::{ShellActionError, open_in_explorer, recycle_delete, rename_path, show_properties};
pub use volume::{VolumeInfo, VolumeKind, WindowsVolumeError, discover_volumes};
