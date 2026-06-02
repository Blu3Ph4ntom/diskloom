//! Windows volume discovery and shell integration.

pub mod elevation;
pub mod scan_broker;
pub mod shell;
pub mod volume;

pub use elevation::{
    ElevationError, is_process_elevated, relaunch_current_process_elevated,
    run_current_process_elevated_and_wait, run_current_process_elevated_hidden_and_wait,
    spawn_current_process_elevated_hidden,
};
pub use scan_broker::{
    ELEVATED_SCAN_TASK_NAME, ElevatedScanRequest, ScanBrokerError, elevated_scan_request_path,
    read_elevated_scan_request, register_elevated_scan_task, remove_elevated_scan_request,
    run_elevated_scan_task, unregister_elevated_scan_task, validate_elevated_scan_output_paths,
    write_elevated_scan_request,
};
pub use shell::{ShellActionError, open_in_explorer, recycle_delete, rename_path, show_properties};
pub use volume::{VolumeInfo, VolumeKind, WindowsVolumeError, discover_volumes};
