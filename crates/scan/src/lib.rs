//! Scanner traits and fallback directory traversal.

pub mod fallback;

pub use fallback::{FallbackScanner, ScanControl, ScanError, ScanOptions, ScanSummary};
