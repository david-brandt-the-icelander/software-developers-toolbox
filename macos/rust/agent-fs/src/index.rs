// A trigram index over one walk's files: it maps every 3-byte sequence to the files containing it, so a search reads only candidates.
use crate::fsio::Ident;
use regex_syntax::hir::literal::{ExtractKind, Extractor};
use regex_syntax::hir::Hir;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_FILE: usize = 2 << 20;
const MAX_ENTRIES: usize = 30_000_000;
const MAX_ALTERNATIVES: usize = 64;
const MAX_TRIGRAMS_PER_ALTERNATIVE: usize = 8;
const SEEN_WORDS: usize = 1 << 18;

thread_local! {
    // Per-thread bitset over all 2^24 trigram codes, used to collect distinct codes without sorting every window.
    static SEEN: RefCell<Vec<u64>> = RefCell::new(vec![0; SEEN_WORDS]);
}

// How a file takes part in searches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Indexed,
    Always,
    Binary,
}

// What a worker learned about one file: its trigrams, or why it cannot be indexed.
pub enum Update {
    Trigrams(Vec<u32>),
    Always,
    Binary,
}

// Where one file sits in the index and how fresh that entry is.
struct Rec {
    id: u32,
    ident: Ident,
    at: i128,
    generation: u64,
}

// The index: file ids, per-trigram posting lists and bookkeeping for incremental updates.
pub struct TriIndex {
    paths: Vec<PathBuf>,
    kinds: Vec<Kind>,
    alive: Vec<bool>,
    by_path: HashMap<PathBuf, Rec>,
    postings: HashMap<u32, Vec<u32>>,
    entries: usize,
    dead: usize,
    generation: u64,
    queries: u32,
    max_entries: usize,
    disabled: bool,
}

// Pack three bytes, folded to ASCII lower case, into one code.
fn code(w: &[u8]) -> u32 {
    (u32::from(w[0].to_ascii_lowercase()) << 16)
        | (u32::from(w[1].to_ascii_lowercase()) << 8)
        | u32::from(w[2].to_ascii_lowercase())
}

// Return the sorted distinct trigram codes of some bytes.
pub fn trigrams(bytes: &[u8]) -> Vec<u32> {
    SEEN.with(|cell| {
        let mut seen = cell.borrow_mut();
        let mut codes = vec![];
        for window in bytes.windows(3) {
            let c = code(window);
            let (word, bit) = ((c >> 6) as usize, 1u64 << (c & 63));
            if seen[word] & bit == 0 {
                seen[word] |= bit;
                codes.push(c);
            }
        }
        for &c in &codes {
            seen[(c >> 6) as usize] = 0;
        }
        codes.sort_unstable();
        codes
    })
}

impl Update {
    // Decide how a freshly read file is indexed; files too big, binary or too new to trust are never filtered.
    pub fn classify(bytes: &[u8], ident: &Ident, at: i128, binary: bool) -> Update {
        match () {
            _ if binary => Update::Binary,
            _ if bytes.len() > MAX_FILE || ident.racy(at) => Update::Always,
            _ => Update::Trigrams(trigrams(bytes)),
        }
    }
}

// Return the trigram sets of the literals every match must start (or end) with, or None when unusable.
fn alternatives(hir: &Hir, kind: ExtractKind) -> Option<Vec<Vec<u32>>> {
    let mut extractor = Extractor::new();
    extractor.kind(kind);
    let seq = extractor.extract(hir);
    let literals = seq.literals()?;
    if literals.is_empty() {
        return None;
    }
    let mut out = vec![];
    for literal in literals {
        if literal.as_bytes().len() < 3 {
            return None;
        }
        out.push(trigrams(literal.as_bytes()));
    }
    out.sort();
    out.dedup();
    (out.len() <= MAX_ALTERNATIVES).then_some(out)
}

// Build a prefilter: a match requires all trigrams of at least one returned alternative.
pub fn plan(hir: &Hir) -> Option<Vec<Vec<u32>>> {
    [ExtractKind::Prefix, ExtractKind::Suffix]
        .into_iter()
        .filter_map(|kind| alternatives(hir, kind))
        .max_by_key(|alts| alts.iter().map(Vec::len).min().unwrap_or(0))
}

impl TriIndex {
    // Create an empty, usable index with the default memory budget.
    pub fn new() -> TriIndex {
        TriIndex::with_limit(MAX_ENTRIES)
    }

    // Create an empty index that disables itself above the given number of posting entries.
    pub fn with_limit(max_entries: usize) -> TriIndex {
        TriIndex {
            max_entries,
            paths: vec![],
            kinds: vec![],
            alive: vec![],
            by_path: HashMap::new(),
            postings: HashMap::new(),
            entries: 0,
            dead: 0,
            generation: 0,
            queries: 0,
            disabled: false,
        }
    }

    // Return false once the index outgrew its memory budget and was dropped.
    pub fn usable(&self) -> bool {
        !self.disabled
    }

    // Count a search and return true once the index is worth building, which is from the second search on.
    pub fn warm(&mut self) -> bool {
        self.queries += 1;
        self.queries > 1
    }

    // Start an update for the files of a walk and return the positions that need reading.
    pub fn begin(&mut self, files: &[(PathBuf, Ident)]) -> Vec<usize> {
        self.generation += 1;
        let mut stale = vec![];
        for (i, (path, ident)) in files.iter().enumerate() {
            match self.by_path.get_mut(path) {
                Some(r) if r.ident == *ident && !r.ident.racy(r.at) => r.generation = self.generation,
                _ => stale.push(i),
            }
        }
        stale
    }

    // Mark an id dead; its postings are dropped at the next compaction.
    fn retire(&mut self, id: u32) {
        self.alive[id as usize] = false;
        self.dead += 1;
    }

    // Record what was learned about a file, replacing any older entry.
    pub fn apply(&mut self, path: &Path, ident: Ident, at: i128, update: Update) {
        if let Some(old) = self.by_path.get(path) {
            self.retire(old.id);
        }
        let id = self.paths.len() as u32;
        let kind = match update {
            Update::Trigrams(codes) => {
                self.entries += codes.len();
                for c in codes {
                    self.postings.entry(c).or_default().push(id);
                }
                Kind::Indexed
            }
            Update::Always => Kind::Always,
            Update::Binary => Kind::Binary,
        };
        self.paths.push(path.to_path_buf());
        self.kinds.push(kind);
        self.alive.push(true);
        self.by_path.insert(path.to_path_buf(), Rec { id, ident, at, generation: self.generation });
    }

    // Drop files that left the walk, compact when many entries are dead, and enforce the memory budget.
    pub fn finish(&mut self) {
        let (generation, mut gone) = (self.generation, vec![]);
        self.by_path.retain(|_, r| {
            let keep = r.generation == generation;
            if !keep {
                gone.push(r.id);
            }
            keep
        });
        for id in gone {
            self.retire(id);
        }
        if self.entries > self.max_entries {
            *self = TriIndex { disabled: true, ..TriIndex::with_limit(self.max_entries) };
        } else if self.dead > (self.paths.len() - self.dead) / 2 + 1024 {
            self.compact();
        }
    }

    // Rebuild the id space without dead files.
    fn compact(&mut self) {
        let mut remap = vec![u32::MAX; self.paths.len()];
        let (mut paths, mut kinds) = (vec![], vec![]);
        for (old, &alive) in self.alive.iter().enumerate() {
            if alive {
                remap[old] = paths.len() as u32;
                paths.push(std::mem::take(&mut self.paths[old]));
                kinds.push(self.kinds[old]);
            }
        }
        for list in self.postings.values_mut() {
            list.retain_mut(|id| {
                *id = remap[*id as usize];
                *id != u32::MAX
            });
        }
        self.postings.retain(|_, list| !list.is_empty());
        self.entries = self.postings.values().map(Vec::len).sum();
        for r in self.by_path.values_mut() {
            r.id = remap[r.id as usize];
        }
        self.alive = vec![true; paths.len()];
        (self.paths, self.kinds, self.dead) = (paths, kinds, 0);
    }

    // Return the ids containing every trigram of one alternative.
    fn matching(&self, alternative: &[u32]) -> Vec<u32> {
        let mut lists: Vec<&Vec<u32>> = Vec::with_capacity(alternative.len());
        for c in alternative {
            match self.postings.get(c) {
                Some(list) => lists.push(list),
                None => return vec![],
            }
        }
        lists.sort_by_key(|l| l.len());
        lists.truncate(MAX_TRIGRAMS_PER_ALTERNATIVE);
        let mut ids = lists[0].clone();
        for list in &lists[1..] {
            ids.retain(|id| list.binary_search(id).is_ok());
        }
        ids
    }

    // Return the paths that may match: those passing the plan, plus every file that is never filtered.
    pub fn candidates(&self, plan: Option<&[Vec<u32>]>) -> Vec<PathBuf> {
        let mut ids: Vec<u32> = match plan {
            None => {
                (0..self.paths.len() as u32).filter(|&i| self.kinds[i as usize] != Kind::Binary).collect()
            }
            Some(alts) => {
                let mut ids: Vec<u32> = alts.iter().flat_map(|a| self.matching(a)).collect();
                ids.extend((0..self.paths.len() as u32).filter(|&i| self.kinds[i as usize] == Kind::Always));
                ids
            }
        };
        ids.retain(|&i| self.alive[i as usize]);
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().map(|i| self.paths[i as usize].clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex_syntax::ParserBuilder;

    // Parse a pattern the way search does.
    fn hir(pattern: &str, ignore_case: bool) -> Hir {
        ParserBuilder::new().case_insensitive(ignore_case).multi_line(true).build().parse(pattern).unwrap()
    }

    // Build an ident from a real temporary file.
    fn ident() -> Ident {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "x").unwrap();
        crate::fsio::ident_of(&p).unwrap()
    }

    // Trigrams are distinct, sorted and case-folded.
    #[test]
    fn test_trigrams_fold_and_dedup() {
        assert_eq!(trigrams(b"abcABC"), trigrams(b"abcabc"));
        assert_eq!(trigrams(b"aaaa").len(), 1);
        assert!(trigrams(b"ab").is_empty());
    }

    // Literals give a plan; short, wildcard-led and empty patterns give none.
    #[test]
    fn test_plan_usability() {
        assert_eq!(plan(&hir("hello", false)).unwrap().len(), 1);
        assert_eq!(plan(&hir("foo|barbaz", false)).unwrap().len(), 2);
        assert!(plan(&hir("ab", false)).is_none());
        assert!(plan(&hir(r"\w+", false)).is_none());
        assert!(plan(&hir("a*", false)).is_none());
        assert_eq!(plan(&hir(r"\w+needle", false)).unwrap().len(), 1);
        assert!(plan(&hir("(?i)needle", true)).is_some());
    }

    // Candidates are the files holding all trigrams, always-files and nothing binary.
    #[test]
    fn test_candidates_filtering() {
        let mut idx = TriIndex::new();
        let id = ident();
        idx.begin(&[]);
        idx.apply(Path::new("a"), id, 0, Update::Trigrams(trigrams(b"hello world")));
        idx.apply(Path::new("b"), id, 0, Update::Trigrams(trigrams(b"goodbye")));
        idx.apply(Path::new("c"), id, 0, Update::Always);
        idx.apply(Path::new("d"), id, 0, Update::Binary);
        idx.finish();
        let p = plan(&hir("hello", false)).unwrap();
        assert_eq!(idx.candidates(Some(&p)), vec![PathBuf::from("a"), PathBuf::from("c")]);
        assert_eq!(idx.candidates(None).len(), 3);
        let p = plan(&hir("(?i)WORLD|bye", true)).unwrap();
        assert_eq!(
            idx.candidates(Some(&p)),
            vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]
        );
    }

    // Changed files are re-read, removed files vanish and compaction keeps results intact.
    #[test]
    fn test_incremental_update_and_compaction() {
        let id = ident();
        let at = crate::fsio::now_ns() + 10_000_000_000;
        let mut idx = TriIndex::new();
        let files: Vec<(PathBuf, Ident)> = (0..3000).map(|i| (PathBuf::from(format!("f{i}")), id)).collect();
        let stale = idx.begin(&files);
        assert_eq!(stale.len(), 3000);
        for (p, i) in &files {
            idx.apply(p, *i, at, Update::Trigrams(trigrams(format!("token{p:?}").as_bytes())));
        }
        idx.finish();
        let keep = &files[..10];
        assert!(idx.begin(keep).is_empty());
        idx.finish();
        assert_eq!(idx.paths.len(), 10);
        let p = plan(&hir("token", false)).unwrap();
        assert_eq!(idx.candidates(Some(&p)).len(), 10);
        let stale = idx.begin(&files[..11]);
        assert_eq!(stale, vec![10]);
    }

    // Classification covers text, big, too-recent and binary files.
    #[test]
    fn test_classify_cases() {
        let id = ident();
        let later = crate::fsio::now_ns() + 10_000_000_000;
        assert!(matches!(Update::classify(b"hello", &id, later, false), Update::Trigrams(_)));
        assert!(matches!(Update::classify(&vec![b'a'; MAX_FILE + 1], &id, later, false), Update::Always));
        assert!(matches!(Update::classify(b"hello", &id, 0, false), Update::Always));
        assert!(matches!(Update::classify(b"hello", &id, later, true), Update::Binary));
    }

    // Empty literal sets, too many alternatives and folded case variants are handled.
    #[test]
    fn test_plan_limits() {
        assert!(plan(&hir(r"[^\s\S]", false)).is_none());
        assert_eq!(plan(&hir("(?i)abcdefg", true)).unwrap().len(), 1);
        let many = (0..70).map(|i| format!("w{i:03}x")).collect::<Vec<_>>().join("|");
        assert!(plan(&hir(&many, false)).is_none());
    }

    // A trigram nobody has yields no candidates.
    #[test]
    fn test_candidates_missing_trigram() {
        let mut idx = TriIndex::new();
        idx.begin(&[]);
        idx.apply(Path::new("a"), ident(), 0, Update::Trigrams(trigrams(b"hello")));
        idx.finish();
        assert!(idx.candidates(Some(&plan(&hir("zzzz", false)).unwrap())).is_empty());
    }

    // Exceeding the entry budget drops the index and marks it unusable.
    #[test]
    fn test_budget_disables_index() {
        let mut idx = TriIndex::with_limit(2);
        assert!(idx.usable());
        idx.begin(&[]);
        idx.apply(Path::new("a"), ident(), 0, Update::Trigrams(trigrams(b"hello world")));
        idx.finish();
        assert!(!idx.usable());
        assert!(idx.candidates(None).is_empty());
    }

    // The index only asks to be built from the second search on.
    #[test]
    fn test_warm_after_first_query() {
        let mut idx = TriIndex::new();
        assert!(!idx.warm());
        assert!(idx.warm());
    }
}
