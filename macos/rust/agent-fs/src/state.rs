// Session state shared by all operations: what was sent, file stamps, stored contents and cached results.
use crate::fsio::{self, Ident};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const STORE_BUDGET: usize = 64 << 20;
const STORE_MAX_FILE: usize = 8 << 20;
const CACHE_MAX: usize = 256;
const TABLE_MAX: usize = 200_000;

// Result type used by every operation; the error is a plain message for the agent.
pub type Res<T> = Result<T, String>;

// A path with its stamp at the time a result was computed, or None when it was absent.
pub type Dep = (PathBuf, Option<Ident>);

// What this session has already received for one subject (a path or a query).
pub struct Sent {
    etag: String,
    keys: HashMap<String, String>,
    complete: bool,
}

// A cached operation result with the files it depends on.
struct Entry {
    deps: Vec<Dep>,
    value: Value,
    etag: String,
    trusted: bool,
}

// All mutable state of one agent session.
pub struct State {
    next_ref: u64,
    sent: HashMap<String, Sent>,
    store: HashMap<String, Arc<Vec<u8>>>,
    order: VecDeque<String>,
    store_bytes: usize,
    idx: HashMap<PathBuf, (Ident, String, i128)>,
    cache: HashMap<String, Entry>,
}

impl State {
    // Create an empty session.
    pub fn new() -> State {
        State {
            next_ref: 0,
            sent: HashMap::new(),
            store: HashMap::new(),
            order: VecDeque::new(),
            store_bytes: 0,
            idx: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    // Return a fresh reference id for a reply.
    fn new_ref(&mut self) -> String {
        self.next_ref += 1;
        format!("r{}", self.next_ref)
    }

    // Return the known etag of a file when its stamp is unchanged and old enough to trust.
    pub fn trusted_etag(&self, path: &Path, id: &Ident) -> Option<String> {
        let (known, etag, at) = self.idx.get(path)?;
        (known == id && !id.racy(*at)).then(|| etag.clone())
    }

    // Hash freshly read bytes, remember the file's stamp and return the etag.
    pub fn record(&mut self, path: &Path, id: Ident, bytes: &[u8]) -> String {
        let etag = fsio::etag(bytes);
        if self.idx.len() > TABLE_MAX {
            self.idx.clear();
        }
        self.idx.insert(path.to_path_buf(), (id, etag.clone(), fsio::now_ns()));
        etag
    }

    // Return the reference of an earlier reply when this session already received the same thing.
    pub fn already_sent(&self, subject: &str, etag: &str, key: &str) -> Option<String> {
        let sent = self.sent.get(subject)?;
        if sent.etag != etag {
            return None;
        }
        sent.keys.get(key).cloned()
    }

    // Note that a reply was sent and return its new reference id.
    pub fn mark_sent(&mut self, subject: &str, etag: &str, key: &str, complete: bool) -> String {
        let reference = self.new_ref();
        if self.sent.len() > TABLE_MAX {
            self.sent.clear();
        }
        match self.sent.get_mut(subject) {
            Some(s) if s.etag == etag => {
                s.keys.insert(key.to_string(), reference.clone());
                s.complete |= complete;
            }
            _ => {
                let keys = HashMap::from([(key.to_string(), reference.clone())]);
                self.sent.insert(subject.to_string(), Sent { etag: etag.to_string(), keys, complete });
            }
        }
        reference
    }

    // Return the etag of the full version of a path this session last received, if it got all of it.
    pub fn complete_etag(&self, subject: &str) -> Option<String> {
        self.sent.get(subject).filter(|s| s.complete).map(|s| s.etag.clone())
    }

    // Keep a file's bytes as a diff base, evicting the oldest when over budget.
    pub fn put_content(&mut self, etag: &str, bytes: Arc<Vec<u8>>) {
        if bytes.len() > STORE_MAX_FILE || self.store.contains_key(etag) {
            return;
        }
        self.store_bytes += bytes.len();
        self.store.insert(etag.to_string(), bytes);
        self.order.push_back(etag.to_string());
        while self.store_bytes > STORE_BUDGET {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(b) = self.store.remove(&old) {
                self.store_bytes -= b.len();
            }
        }
    }

    // Return stored bytes for an etag.
    pub fn content(&self, etag: &str) -> Option<Arc<Vec<u8>>> {
        self.store.get(etag).cloned()
    }

    // Return a cached result when every dependency is unchanged, else compute and cache it.
    fn cached(
        &mut self,
        key: &str,
        compute: impl FnOnce() -> Res<(Value, Vec<Dep>)>,
    ) -> Res<(Value, String, bool)> {
        if let Some(e) = self.cache.get(key) {
            if e.trusted && e.deps.iter().all(|(p, id)| fsio::ident_of(p) == *id) {
                return Ok((e.value.clone(), e.etag.clone(), true));
            }
        }
        let started = fsio::now_ns();
        let (value, deps) = compute()?;
        let etag = fsio::etag(&serde_json::to_vec(&value).expect("json values serialize"));
        let trusted = deps.iter().all(|(_, id)| id.is_none_or(|i| !i.racy(started)));
        if self.cache.len() >= CACHE_MAX {
            self.cache.clear();
        }
        self.cache.insert(key.to_string(), Entry { deps, value: value.clone(), etag: etag.clone(), trusted });
        Ok((value, etag, false))
    }

    // Serve a cacheable query: reuse or compute it, and send only an unchanged marker if already delivered.
    pub fn serve_cached(
        &mut self,
        key: &str,
        fresh: bool,
        compute: impl FnOnce() -> Res<(Value, Vec<Dep>)>,
    ) -> Res<Value> {
        let (mut value, etag, hit) = self.cached(key, compute)?;
        let subject = fsio::etag(key.as_bytes());
        if !fresh {
            if let Some(r) = self.already_sent(&subject, &etag, "") {
                return Ok(json!({"etag": etag, "unchanged": true, "ref": r}));
            }
        }
        let reference = self.mark_sent(&subject, &etag, "", true);
        value["etag"] = json!(etag);
        value["cached"] = json!(hit);
        value["ref"] = json!(reference);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A repeated reply is recognised only for the same etag and key.
    #[test]
    fn test_sent_tracking() {
        let mut st = State::new();
        let r = st.mark_sent("a", "e1", "k", true);
        assert_eq!(st.already_sent("a", "e1", "k"), Some(r));
        assert_eq!(st.already_sent("a", "e2", "k"), None);
        assert_eq!(st.already_sent("a", "e1", "other"), None);
        assert_eq!(st.complete_etag("a"), Some("e1".to_string()));
    }

    // The oldest stored content is evicted once the budget is exceeded.
    #[test]
    fn test_store_evicts_oldest() {
        let mut st = State::new();
        for i in 0..12 {
            st.put_content(&format!("e{i}"), Arc::new(vec![0u8; 7 << 20]));
        }
        assert!(st.content("e0").is_none());
        assert!(st.content("e11").is_some());
    }
}
