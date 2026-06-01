use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::SystemTime,
};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let launcher_dir = current_exe_dir().unwrap_or_else(|| repo_root.clone());
    let (binary, args) = match launch_target(env::args_os().skip(1)) {
        LaunchTarget::Help => {
            print_help();
            return Ok(ExitCode::SUCCESS);
        }
        LaunchTarget::Run { binary, args } => (binary, args),
    };
    let exe = resolve_launch_binary(&repo_root, &launcher_dir, binary)?;
    let status = Command::new(&exe)
        .args(args)
        .status()
        .map_err(|error| format!("failed to launch {}: {error}", exe.display()))?;

    Ok(exit_code_from_status(status.code()))
}

#[derive(Debug, PartialEq, Eq)]
enum LaunchTarget {
    Run {
        binary: &'static str,
        args: Vec<OsString>,
    },
    Help,
}

fn launch_target(args: impl IntoIterator<Item = OsString>) -> LaunchTarget {
    let mut args = args.into_iter().collect::<Vec<_>>();
    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return LaunchTarget::Run {
            binary: "diskloom-ui",
            args,
        };
    };

    match first {
        "-h" | "--help" | "help" => LaunchTarget::Help,
        "ui" | "--ui" => {
            args.remove(0);
            LaunchTarget::Run {
                binary: "diskloom-ui",
                args,
            }
        }
        "cli" | "--cli" => {
            args.remove(0);
            LaunchTarget::Run {
                binary: "diskloom-cli",
                args,
            }
        }
        "bench" | "--bench" => {
            args.remove(0);
            LaunchTarget::Run {
                binary: "diskloom-bench",
                args,
            }
        }
        "scan" | "volumes" | "ntfs-probe" => LaunchTarget::Run {
            binary: "diskloom-cli",
            args,
        },
        _ => LaunchTarget::Run {
            binary: "diskloom-ui",
            args,
        },
    }
}

fn print_help() {
    println!(
        "DiskLoom\n\nUsage:\n  diskloom.exe                 Launch GUI\n  diskloom.exe ui [args]       Launch GUI\n  diskloom.exe scan <path> ... Run CLI scan\n  diskloom.exe volumes         List Windows volumes\n  diskloom.exe ntfs-probe <v>  Probe NTFS volume\n  diskloom.exe cli <command>   Run CLI command\n  diskloom.exe bench <command> Run benchmark command"
    );
}

fn current_exe_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn resolve_launch_binary(
    repo_root: &Path,
    launcher_dir: &Path,
    binary: &str,
) -> Result<PathBuf, String> {
    let sibling = binary_path(launcher_dir, binary);
    if sibling.exists() {
        return Ok(sibling);
    }

    ensure_release_binary(repo_root, binary)
}

fn ensure_release_binary(repo_root: &Path, binary: &str) -> Result<PathBuf, String> {
    let exe = binary_path(&repo_root.join("target").join("release"), binary);
    if release_binary_is_current(repo_root, &exe)? {
        return Ok(exe);
    }

    eprintln!("Building DiskLoom release binaries...");
    let status = Command::new("cargo")
        .current_dir(repo_root)
        .env("CARGO_BUILD_JOBS", "1")
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "diskloom-launcher",
            "-p",
            "diskloom-cli",
            "-p",
            "diskloom-ui",
            "-p",
            "diskloom-bench",
        ])
        .status()
        .map_err(|error| format!("failed to start cargo build: {error}"))?;

    if !status.success() {
        return Err("release build failed".to_owned());
    }
    if !exe.exists() {
        return Err(format!("release binary was not created: {}", exe.display()));
    }

    Ok(exe)
}

fn binary_path(dir: &Path, binary: &str) -> PathBuf {
    dir.join(format!("{binary}{}", env::consts::EXE_SUFFIX))
}

fn release_binary_is_current(repo_root: &Path, exe: &Path) -> Result<bool, String> {
    let Ok(exe_modified) = fs::metadata(exe).and_then(|metadata| metadata.modified()) else {
        return Ok(false);
    };

    build_inputs_newer_than(repo_root, exe_modified).map(|newer| !newer)
}

fn build_inputs_newer_than(root: &Path, timestamp: SystemTime) -> Result<bool, String> {
    let mut pending = vec![root.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;

            if file_type.is_dir() {
                if !should_skip_build_dir(&path) {
                    pending.push(path);
                }
                continue;
            }

            if is_build_input(&path)
                && fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?
                    > timestamp
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn should_skip_build_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".agent" | ".git" | "dist" | "target"))
}

fn is_build_input(path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
        return true;
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "Cargo.lock" | "Cargo.toml"))
}

fn exit_code_from_status(code: Option<i32>) -> ExitCode {
    match code {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(code.clamp(1, 255) as u8),
        None => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        LaunchTarget, exit_code_from_status, is_build_input, launch_target, resolve_launch_binary,
        should_skip_build_dir,
    };

    #[test]
    fn launch_target_should_default_to_gui() {
        assert_eq!(
            launch_target(Vec::<OsString>::new()),
            LaunchTarget::Run {
                binary: "diskloom-ui",
                args: Vec::new()
            }
        );
    }

    #[test]
    fn launch_target_should_route_cli_prefix() {
        assert_eq!(
            launch_target([OsString::from("cli"), OsString::from("scan")]),
            LaunchTarget::Run {
                binary: "diskloom-cli",
                args: vec![OsString::from("scan")]
            }
        );
    }

    #[test]
    fn launch_target_should_route_cli_subcommands() {
        assert_eq!(
            launch_target([OsString::from("scan"), OsString::from(".")]),
            LaunchTarget::Run {
                binary: "diskloom-cli",
                args: vec![OsString::from("scan"), OsString::from(".")]
            }
        );
    }

    #[test]
    fn launch_target_should_pass_unknown_args_to_gui() {
        assert_eq!(
            launch_target([OsString::from("--path"), OsString::from("C:\\")]),
            LaunchTarget::Run {
                binary: "diskloom-ui",
                args: vec![OsString::from("--path"), OsString::from("C:\\")]
            }
        );
    }

    #[test]
    fn launch_target_should_handle_help_without_gui() {
        assert_eq!(
            launch_target([OsString::from("--help")]),
            LaunchTarget::Help
        );
    }

    #[test]
    fn exit_code_from_status_should_clamp_process_codes() {
        assert_eq!(
            exit_code_from_status(Some(0)),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(
            exit_code_from_status(Some(300)),
            std::process::ExitCode::from(255)
        );
    }

    #[test]
    fn build_input_detection_should_track_rust_and_manifest_files() {
        assert!(is_build_input(std::path::Path::new("src/main.rs")));
        assert!(is_build_input(std::path::Path::new("Cargo.toml")));
        assert!(is_build_input(std::path::Path::new("Cargo.lock")));
        assert!(!is_build_input(std::path::Path::new("README.md")));
    }

    #[test]
    fn build_dir_filter_should_skip_generated_and_private_dirs() {
        assert!(should_skip_build_dir(std::path::Path::new("target")));
        assert!(should_skip_build_dir(std::path::Path::new(".git")));
        assert!(should_skip_build_dir(std::path::Path::new(".agent")));
        assert!(should_skip_build_dir(std::path::Path::new("dist")));
        assert!(!should_skip_build_dir(std::path::Path::new("crates")));
    }

    #[test]
    fn resolve_launch_binary_should_prefer_portable_sibling_binary() {
        let temp_dir = std::env::temp_dir().join(format!(
            "diskloom-launcher-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let expected = temp_dir.join(format!("diskloom-ui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&expected, b"").unwrap();

        let resolved =
            resolve_launch_binary(std::path::Path::new("missing"), &temp_dir, "diskloom-ui")
                .unwrap();
        std::fs::remove_dir_all(&temp_dir).unwrap();

        assert_eq!(resolved, expected);
    }
}
