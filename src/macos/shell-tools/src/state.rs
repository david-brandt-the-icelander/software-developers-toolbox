// Session memory for JSON replies: what each session was sent, and the file stamps and contents all sessions share.
use crate::fsio::{self, Ident};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const STORE_BUDGET: usize = 64 << 20;
const STORE_MAX_FILE: usize = 8 << 20;
const TABLE_MAX: usize = 200_000;

// Result type used by every operation; the error is a plain message for the agent.
pub type Res<T> = Result<T, String>;

// What a session has already received for one subject (a path or a query).
struct Subject {
    etag: String,
    keys: HashMap<String, String>,
    complete: bool,
}

// What one session has been sent, and the counter behind its reply references.
#[derive(Default)]
pub struct Sent {
    next_ref: u64,
    subjects: HashMap<String, Subject>,
}

// What all sessions share: trusted file stamps with their etags, and file contents kept as diff bases.
#[derive(Default)]
pub struct Shared {
    store: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    store_bytes: usize,
    idx: HashMap<PathBuf, (Ident, String, i128)>,
}

// One request's view of the memory: the shared part and the asking session's part.
pub struct State<'a> {
    pub shared: &'a mut Shared,
    pub sent: &'a mut Sent,
}

impl Sent {
    // Returns the reference of an earlier reply when this session already received the same thing.
    pub fn already_sent(&self, subject: &str, etag: &str, key: &str) -> Option<String> {
        let s = self.subjects.get(subject).filter(|s| s.etag == etag)?;
        s.keys.get(key).cloned()
    }

    // Notes that a reply was sent and returns its new reference id.
    pub fn mark_sent(&mut self, subject: &str, etag: &str, key: &str, complete: bool) -> String {
        self.next_ref += 1;
        let reference = format!("r{}", self.next_ref);
        if self.subjects.len() > TABLE_MAX {
            self.subjects.clear();
        }
        match self.subjects.get_mut(subject) {
            Some(s) if s.etag == etag => {
                s.keys.insert(key.to_string(), reference.clone());
                s.complete |= complete;
            }
            _ => {
                let keys = HashMap::from([(key.to_string(), reference.clone())]);
                self.subjects.insert(subject.to_string(), Subject { etag: etag.to_string(), keys, complete });
            }
        }
        reference
    }

    // Returns the etag of the version of a subject this session last received whole, if it did.
    pub fn complete_etag(&self, subject: &str) -> Option<String> {
        self.subjects.get(subject).filter(|s| s.complete).map(|s| s.etag.clone())
    }
}

impl Shared {
    // Returns the known etag of a file when its stamp is unchanged and old enough to trust.
    pub fn trusted_etag(&self, path: &Path, id: &Ident) -> Option<String> {
        let (known, etag, at) = self.idx.get(path)?;
        (known == id && !id.racy(*at)).then(|| etag.clone())
    }

    // Hashes freshly read bytes, remembers the file's stamp and returns the etag.
    pub fn record(&mut self, path: &Path, id: Ident, bytes: &[u8]) -> String {
        let etag = fsio::etag(bytes);
        if self.idx.len() > TABLE_MAX {
            self.idx.clear();
        }
        self.idx.insert(path.to_path_buf(), (id, etag.clone(), fsio::now_ns()));
        etag
    }

    // Keeps a file's bytes as a diff base, evicting the oldest when over budget.
    pub fn put_content(&mut self, etag: &str, bytes: Arc<Vec<u8>>) {
        if bytes.len() > STORE_MAX_FILE || self.store.contains_key(etag) {
            return;
        }
        self.store_bytes += bytes.len();
        self.store.insert(etag.to_string(), bytes);
        self.order.push_back(etag.to_string());
        while self.store_bytes > STORE_BUDGET {
            let old = self.order.pop_front().expect("stored bytes imply queued etags");
            let bytes = self.store.remove(&old).expect("the queue only holds stored etags");
            self.store_bytes -= bytes.len();
        }
    }

    // Returns stored bytes for an etag.
    pub fn content(&self, etag: &str) -> Option<Arc<Vec<u8>>> {
        self.store.get(etag).cloned()
    }
}

impl State<'_> {
    // Sends a query's result, or only an unchanged marker when this session already received the same result.
    pub fn serve(&mut self, key: &str, fresh: bool, mut value: Value) -> Value {
        let etag = fsio::etag(&serde_json::to_vec(&value).expect("json values serialize"));
        let subject = fsio::etag(key.as_bytes());
        if let Some(r) = self.sent.already_sent(&subject, &etag, "").filter(|_| !fresh) {
            return json!({"etag": etag, "unchanged": true, "ref": r});
        }
        value["ref"] = json!(self.sent.mark_sent(&subject, &etag, "", true));
        value["etag"] = json!(etag);
        value
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // A repeated reply is recognised only for the same etag and key.
    #[test]
    fn test_sent_tracking() {
        let mut sent = Sent::default();
        let r = sent.mark_sent("a", "e1", "k", true);
        assert_eq!(sent.already_sent("a", "e1", "k"), Some(r));
        assert_eq!(sent.already_sent("a", "e2", "k"), None);
        assert_eq!(sent.already_sent("a", "e1", "other"), None);
        assert_eq!(sent.complete_etag("a"), Some("e1".to_string()));
        sent.mark_sent("a", "e1", "k2", false);
        assert!(sent.already_sent("a", "e1", "k2").is_some());
    }

    // The oldest stored content is evicted once the budget is exceeded.
    #[test]
    fn test_store_evicts_oldest() {
        let mut sh = Shared::default();
        for i in 0..12 {
            sh.put_content(&format!("e{i}"), Arc::new(vec![0u8; 7 << 20]));
        }
        assert!(sh.content("e0").is_none());
        assert!(sh.content("e11").is_some());
    }

    // A repeat of a delivered result is only a marker until it changes or fresh is asked.
    #[test]
    fn test_serve_unchanged_marker() {
        let (mut sh, mut sent) = (Shared::default(), Sent::default());
        let mut st = State { shared: &mut sh, sent: &mut sent };
        assert!(st.serve("q", false, json!({"v": 1})).get("v").is_some());
        assert_eq!(st.serve("q", false, json!({"v": 1}))["unchanged"], true);
        assert!(st.serve("q", true, json!({"v": 1})).get("v").is_some());
        assert!(st.serve("q", false, json!({"v": 2})).get("v").is_some());
    }

    // Oversized and duplicate contents are not stored twice or at all.
    #[test]
    fn test_put_content_rules() {
        let mut sh = Shared::default();
        sh.put_content("big", Arc::new(vec![0u8; STORE_MAX_FILE + 1]));
        assert!(sh.content("big").is_none());
        sh.put_content("a", Arc::new(vec![1]));
        sh.put_content("a", Arc::new(vec![2]));
        assert_eq!(sh.content("a").unwrap().as_slice(), [1]);
    }

    // The stamp and sent tables reset when they grow past their limit.
    #[test]
    fn test_table_limits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "x").unwrap();
        let id = fsio::ident_of(&p).unwrap();
        let mut sh = Shared::default();
        sh.record(Path::new("first"), id, b"");
        for i in 0..=TABLE_MAX {
            sh.record(Path::new(&format!("p{i}")), id, b"");
        }
        assert!(!sh.idx.contains_key(Path::new("first")));
        let mut sent = Sent::default();
        sent.mark_sent("first", "e", "k", true);
        for i in 0..=TABLE_MAX {
            sent.mark_sent(&format!("s{i}"), "e", "k", true);
        }
        assert!(sent.already_sent("first", "e", "k").is_none());
    }

    // A stamp is trusted for skipping a re-read only after it is older than the racy window.
    #[test]
    fn test_trusted_etag_requires_old_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "same-size-1").unwrap();
        let mut sh = Shared::default();
        let id = fsio::ident_of(&p).unwrap();
        let etag = sh.record(&p, id, b"same-size-1");
        assert!(sh.trusted_etag(&p, &id).is_none());
        std::thread::sleep(std::time::Duration::from_millis(2200));
        sh.record(&p, id, b"same-size-1");
        assert_eq!(sh.trusted_etag(&p, &id), Some(etag));
        let other = fsio::Ident::of(&std::fs::metadata(dir.path()).unwrap());
        assert!(sh.trusted_etag(&p, &other).is_none());
        assert!(sh.trusted_etag(Path::new("unknown"), &id).is_none());
    }
}
