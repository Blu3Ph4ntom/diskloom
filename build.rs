use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=frontend/package.json");
    println!("cargo:rerun-if-changed=frontend/package-lock.json");
    rerun_if_changed_recursive(Path::new("frontend/src"));
    rerun_if_changed_recursive(Path::new("frontend/public"));

    if env::var_os("DISKLOOM_SKIP_FRONTEND_BUILD").is_none() {
        ensure_frontend_assets();
    }

    tauri_build::build();
}

fn ensure_frontend_assets() {
    let frontend = PathBuf::from("frontend");
    let dist_index = frontend.join("dist").join("index.html");
    if dist_index.exists() && !frontend_sources_newer_than(&frontend, &dist_index) {
        return;
    }

    if !frontend.join("node_modules").exists() {
        run_npm(&frontend, &["ci"]);
    }
    run_npm(&frontend, &["run", "build"]);
}

fn frontend_sources_newer_than(frontend: &Path, dist_index: &Path) -> bool {
    let Ok(dist_modified) = fs::metadata(dist_index).and_then(|metadata| metadata.modified())
    else {
        return true;
    };

    let mut pending = vec![frontend.join("src"), frontend.join("package.json")];
    pending.push(frontend.join("vite.config.ts"));
    pending.push(frontend.join("tsconfig.json"));

    while let Some(path) = pending.pop() {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
            continue;
        }
        if metadata
            .modified()
            .is_ok_and(|modified| modified > dist_modified)
        {
            return true;
        }
    }

    false
}

fn rerun_if_changed_recursive(path: &Path) {
    if !path.exists() {
        return;
    }
    let mut pending = vec![path.to_path_buf()];
    while let Some(path) = pending.pop() {
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
    }
}

fn run_npm(frontend: &Path, args: &[&str]) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(frontend)
        .status()
        .expect("failed to start npm for DiskLoom frontend build");
    assert!(
        status.success(),
        "DiskLoom frontend npm command failed: npm {}",
        args.join(" ")
    );
}
