fn main() {
    println!("cargo:rerun-if-changed=diskloom-cli.rc");
    println!("cargo:rerun-if-changed=../../icons/icon.ico");

    #[cfg(windows)]
    embed_resource::compile("diskloom-cli.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("failed to embed DiskLoom CLI icon");
}
