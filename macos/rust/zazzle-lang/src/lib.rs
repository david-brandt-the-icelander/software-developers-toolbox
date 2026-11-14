// A tree-sitter grammar packaged as a zazzled language module: built once per language feature, it exports the grammar and the module format version for zazzled to load with dlopen.
#[cfg(feature = "bash")]
use tree_sitter_bash::LANGUAGE as GRAMMAR;
#[cfg(feature = "c")]
use tree_sitter_c::LANGUAGE as GRAMMAR;
#[cfg(feature = "csharp")]
use tree_sitter_c_sharp::LANGUAGE as GRAMMAR;
#[cfg(feature = "clojure")]
use tree_sitter_clojure_orchard::LANGUAGE as GRAMMAR;
#[cfg(feature = "cpp")]
use tree_sitter_cpp::LANGUAGE as GRAMMAR;
#[cfg(feature = "dart")]
use tree_sitter_dart::LANGUAGE as GRAMMAR;
#[cfg(feature = "elixir")]
use tree_sitter_elixir::LANGUAGE as GRAMMAR;
#[cfg(feature = "erlang")]
use tree_sitter_erlang::LANGUAGE as GRAMMAR;
#[cfg(feature = "fortran")]
use tree_sitter_fortran::LANGUAGE as GRAMMAR;
#[cfg(feature = "fsharp")]
use tree_sitter_fsharp::LANGUAGE_FSHARP as GRAMMAR;
#[cfg(feature = "go")]
use tree_sitter_go::LANGUAGE as GRAMMAR;
#[cfg(feature = "groovy")]
use tree_sitter_groovy::LANGUAGE as GRAMMAR;
#[cfg(feature = "haskell")]
use tree_sitter_haskell::LANGUAGE as GRAMMAR;
#[cfg(feature = "java")]
use tree_sitter_java::LANGUAGE as GRAMMAR;
#[cfg(feature = "javascript")]
use tree_sitter_javascript::LANGUAGE as GRAMMAR;
#[cfg(feature = "julia")]
use tree_sitter_julia::LANGUAGE as GRAMMAR;
#[cfg(feature = "kotlin")]
use tree_sitter_kotlin_ng::LANGUAGE as GRAMMAR;
#[cfg(feature = "lua")]
use tree_sitter_lua::LANGUAGE as GRAMMAR;
#[cfg(feature = "objc")]
use tree_sitter_objc::LANGUAGE as GRAMMAR;
#[cfg(feature = "ocaml")]
use tree_sitter_ocaml::LANGUAGE_OCAML as GRAMMAR;
#[cfg(feature = "pascal")]
use tree_sitter_pascal::LANGUAGE as GRAMMAR;
#[cfg(feature = "php")]
use tree_sitter_php::LANGUAGE_PHP as GRAMMAR;
#[cfg(feature = "powershell")]
use tree_sitter_powershell::LANGUAGE as GRAMMAR;
#[cfg(feature = "python")]
use tree_sitter_python::LANGUAGE as GRAMMAR;
#[cfg(feature = "r")]
use tree_sitter_r::LANGUAGE as GRAMMAR;
#[cfg(feature = "ruby")]
use tree_sitter_ruby::LANGUAGE as GRAMMAR;
#[cfg(feature = "rust")]
use tree_sitter_rust::LANGUAGE as GRAMMAR;
#[cfg(feature = "scala")]
use tree_sitter_scala::LANGUAGE as GRAMMAR;
#[cfg(feature = "swift")]
use tree_sitter_swift::LANGUAGE as GRAMMAR;
#[cfg(feature = "tsx")]
use tree_sitter_typescript::LANGUAGE_TSX as GRAMMAR;
#[cfg(feature = "typescript")]
use tree_sitter_typescript::LANGUAGE_TYPESCRIPT as GRAMMAR;
#[cfg(feature = "vb")]
use tree_sitter_vb_dotnet::LANGUAGE as GRAMMAR;
#[cfg(feature = "zig")]
use tree_sitter_zig::LANGUAGE as GRAMMAR;
#[cfg(feature = "perl")]
use ts_parser_perl::LANGUAGE as GRAMMAR;

// The module format version zazzled checks before calling zazzle_language.
#[no_mangle]
pub static ZAZZLE_LANGUAGE_ABI: u32 = 1;

// Returns the grammar's TSLanguage pointer, or null in a build with no language feature.
#[no_mangle]
pub extern "C" fn zazzle_language() -> *const () {
    grammar()
}

// Calls the grammar crate's language function.
#[cfg(any(
    feature = "rust",
    feature = "python",
    feature = "c",
    feature = "javascript",
    feature = "typescript",
    feature = "csharp",
    feature = "fortran",
    feature = "pascal",
    feature = "tsx",
    feature = "bash",
    feature = "dart",
    feature = "go",
    feature = "java",
    feature = "cpp",
    feature = "lua",
    feature = "kotlin",
    feature = "swift",
    feature = "perl",
    feature = "php",
    feature = "ruby",
    feature = "scala",
    feature = "elixir",
    feature = "haskell",
    feature = "ocaml",
    feature = "fsharp",
    feature = "clojure",
    feature = "erlang",
    feature = "r",
    feature = "julia",
    feature = "zig",
    feature = "objc",
    feature = "groovy",
    feature = "powershell",
    feature = "vb"
))]
fn grammar() -> *const () {
    let f: tree_sitter_language::LanguageFn = GRAMMAR;
    // SAFETY: the grammar crate's function only returns a pointer to its static TSLanguage.
    unsafe { f.into_raw()() }
}

// Returns null: this build carries no grammar.
#[cfg(not(any(
    feature = "rust",
    feature = "python",
    feature = "c",
    feature = "javascript",
    feature = "typescript",
    feature = "csharp",
    feature = "fortran",
    feature = "pascal",
    feature = "tsx",
    feature = "bash",
    feature = "dart",
    feature = "go",
    feature = "java",
    feature = "cpp",
    feature = "lua",
    feature = "kotlin",
    feature = "swift",
    feature = "perl",
    feature = "php",
    feature = "ruby",
    feature = "scala",
    feature = "elixir",
    feature = "haskell",
    feature = "ocaml",
    feature = "fsharp",
    feature = "clojure",
    feature = "erlang",
    feature = "r",
    feature = "julia",
    feature = "zig",
    feature = "objc",
    feature = "groovy",
    feature = "powershell",
    feature = "vb"
)))]
fn grammar() -> *const () {
    std::ptr::null()
}
