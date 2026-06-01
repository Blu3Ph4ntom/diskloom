use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
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
    let (binary, args) = launch_target(env::args_os().skip(1));
    let exe = ensure_release_binary(&repo_root, binary)?;
    let status = Command::new(&exe)
        .args(args)
        .status()
        .map_err(|error| format!("failed to launch {}: {error}", exe.display()))?;

    Ok(exit_code_from_status(status.code()))
}

fn launch_target(args: impl IntoIterator<Item = OsString>) -> (&'static str, Vec<OsString>) {
    let mut args = args.into_iter().collect::<Vec<_>>();
    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return ("diskloom-ui", args);
    };

    match first {
        "ui" | "--ui" => {
            args.remove(0);
            ("diskloom-ui", args)
        }
        "cli" | "--cli" => {
            args.remove(0);
            ("diskloom", args)
        }
        "bench" | "--bench" => {
            args.remove(0);
            ("diskloom-bench", args)
        }
        _ => ("diskloom-ui", args),
    }
}

fn ensure_release_binary(repo_root: &Path, binary: &str) -> Result<PathBuf, String> {
    let exe = repo_root
        .join("target")
        .join("release")
        .join(format!("{binary}{}", env::consts::EXE_SUFFIX));
    if exe.exists() {
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

    use super::{exit_code_from_status, launch_target};

    #[test]
    fn launch_target_should_default_to_gui() {
        let (binary, args) = launch_target(Vec::<OsString>::new());

        assert_eq!(binary, "diskloom-ui");
        assert!(args.is_empty());
    }

    #[test]
    fn launch_target_should_route_cli_prefix() {
        let (binary, args) = launch_target([OsString::from("cli"), OsString::from("scan")]);

        assert_eq!(binary, "diskloom");
        assert_eq!(args, [OsString::from("scan")]);
    }

    #[test]
    fn launch_target_should_pass_unknown_args_to_gui() {
        let (binary, args) = launch_target([OsString::from("--path"), OsString::from("C:\\")]);

        assert_eq!(binary, "diskloom-ui");
        assert_eq!(args, [OsString::from("--path"), OsString::from("C:\\")]);
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
}
