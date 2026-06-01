#[cfg(windows)]
mod native;

#[cfg(windows)]
pub use native::run_from_env_args;

#[cfg(not(windows))]
pub fn run_from_env_args() -> anyhow::Result<()> {
    anyhow::bail!("DiskLoom GUI is Windows-only for v1")
}
