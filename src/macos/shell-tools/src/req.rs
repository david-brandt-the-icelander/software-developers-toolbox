// Request schemas: one strict argument struct per operation, parsed from JSON.
use serde::Deserialize;

// Every operation an agent can ask for, selected by the `op` field.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Read(ReadArgs),
    Search(SearchArgs),
    List(ListArgs),
    Stat(StatArgs),
    Outline(OutlineArgs),
    DiffSince(DiffArgs),
}

// Arguments of the read operation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub path: String,
    #[serde(default)]
    pub ranges: Vec<[usize; 2]>,
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub if_none_match: Option<String>,
    #[serde(default)]
    pub fresh: bool,
}

// Arguments of the search operation.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    pub pattern: String,
    #[serde(default)]
    pub literal: bool,
    #[serde(default)]
    pub ignore_case: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub glob: Vec<String>,
    #[serde(default)]
    pub context: usize,
    pub max_matches: Option<usize>,
    #[serde(default)]
    pub files_only: bool,
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub no_ignore: bool,
    #[serde(default)]
    pub fresh: bool,
}

// Arguments of the list operation.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    pub path: Option<String>,
    #[serde(default)]
    pub glob: Vec<String>,
    pub depth: Option<usize>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub no_ignore: bool,
    #[serde(default)]
    pub long: bool,
    pub max_entries: Option<usize>,
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub fresh: bool,
}

// Arguments of the stat operation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatArgs {
    pub path: String,
    #[serde(default)]
    pub hash: bool,
}

// Arguments of the outline operation.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct OutlineArgs {
    #[serde(default)]
    pub paths: Vec<String>,
    pub max_symbols: Option<usize>,
    #[serde(default)]
    pub fresh: bool,
}

// Arguments of the diff_since operation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffArgs {
    pub path: String,
    pub etag: String,
}
