// Compiles the vendored lzfse-zazzcat C library (macos/c/lzfse-zazzcat) into free-disk-space.
use std::path::Path;

// Builds every C source of the library with the flags its own Makefile uses.
fn main() {
    let root = Path::new("../../c/lzfse-zazzcat");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut build = cc::Build::new();
    for entry in std::fs::read_dir(root.join("src")).expect("reading the lzfse-zazzcat sources") {
        let path = entry.expect("listing the lzfse-zazzcat sources").path();
        if path.extension().is_some_and(|e| e == "c") {
            build.file(&path);
        }
    }
    build
        .include(root.join("include"))
        .include(root.join("src"))
        .std("c11")
        .define("_DEFAULT_SOURCE", None)
        .opt_level(3)
        .compile("lzfse_zazzcat");
}
