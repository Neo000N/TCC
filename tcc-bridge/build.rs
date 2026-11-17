use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // Only build the openpty polyfill for Android targets
    if target_os == "android" || target.contains("android") {
        println!("cargo:rustc-link-search=native={}", env::var("OUT_DIR").unwrap());

        // Compile the C polyfill
        cc::Build::new()
            .file("src/pty-android/openpty.c")
            .compile("openpty_android");

        println!("cargo:rerun-if-changed=src/pty-android/openpty.c");
    }

    // Tell cargo to rerun if build.rs changes
    println!("cargo:rerun-if-changed=build.rs");
}