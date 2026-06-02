fn main() {
    println!("cargo:rerun-if-changed=diskloom-setup.rc");
    println!("cargo:rerun-if-changed=diskloom-setup-debug.rc");
    println!("cargo:rerun-if-changed=diskloom-setup.manifest");
    println!("cargo:rerun-if-changed=diskloom-setup-debug.manifest");
    println!("cargo:rerun-if-changed=../../icons/icon.ico");

    #[cfg(windows)]
    {
        let resource = if std::env::var("PROFILE").is_ok_and(|profile| profile == "release") {
            "diskloom-setup.rc"
        } else {
            "diskloom-setup-debug.rc"
        };
        embed_resource::compile(resource, embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed DiskLoom setup resources");
    }
}
