// Embeds the language module manifest named by ZAZZLED_LANGUAGES_MANIFEST (empty when unset), so zazzled accepts only the modules built with it.
use std::path::PathBuf;

// Copies the manifest, or an empty one, into OUT_DIR for include_str!.
fn main() {
    println!("cargo:rerun-if-env-changed=ZAZZLED_LANGUAGES_MANIFEST");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("languages.manifest");
    let text = match std::env::var_os("ZAZZLED_LANGUAGES_MANIFEST") {
        Some(path) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
            std::fs::read_to_string(&path).expect("reading ZAZZLED_LANGUAGES_MANIFEST")
        }
        None => String::new(),
    };
    std::fs::write(out, text).expect("writing the embedded language manifest");
}
