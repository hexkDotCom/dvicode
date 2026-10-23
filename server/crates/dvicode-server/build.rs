//! On Windows the server runs on the segment heap (see windows.manifest): with the default
//! heap, allocating and freeing the large token buffers of thousands of files across threads
//! gets many times slower once a few snapshots of a big project have come and gone; a header
//! edit that 20,000 files include took 17 s to compile again instead of about 1 s.

fn main() {
    println!("cargo:rerun-if-changed=windows.manifest");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
}
