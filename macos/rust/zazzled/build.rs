// Embeds the language module and JIT tool manifests named by ZAZZLED_LANGUAGES_MANIFEST and ZAZZLED_TOOLS_MANIFEST (empty when unset), so zazzled accepts only the modules and tools built with it.
use std::path::PathBuf;

// Copies the manifest a variable names, or an empty one, into OUT_DIR as `file` for include_str!.
fn embed(var: &str, file: &str) {
    println!("cargo:rerun-if-env-changed={var}");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join(file);
    let text = match std::env::var_os(var) {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {var}: {e}"))
        }
        None => String::new(),
    };
    std::fs::write(out, text).unwrap_or_else(|e| panic!("writing the embedded {file}: {e}"));
}

// Embeds both manifests.
fn main() {
    embed("ZAZZLED_LANGUAGES_MANIFEST", "languages.manifest");
    embed("ZAZZLED_TOOLS_MANIFEST", "tools.manifest");
}
