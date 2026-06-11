use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src_dir = manifest_dir.join("src");

    cxx_build::bridge("src/bridge.rs")
        .file("src/bridge.cpp")
        .flag_if_supported("-std=c++17")
        .flag_if_supported("-Wno-maybe-uninitialized")
        .include(&src_dir)
        .compile("monsoon-bridge");

    println!("cargo:rerun-if-changed=src/bridge.rs");
    println!("cargo:rerun-if-changed=src/bridge.h");
    println!("cargo:rerun-if-changed=src/bridge.cpp");
    println!("cargo:rustc-link-lib=torrent-rasterbar");
}
