// Per-session response tracking: short ids for responses and "already sent" notices for repeats.
use crate::tokens;
use std::collections::HashMap;
use std::sync::Mutex;

pub const WINDOW_TOKENS: u64 = 40_000;
const MAX_SESSIONS: usize = 64;
const MAX_ENTRIES: usize = 512;
const MIN_ID: usize = 5;
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

// What one session has been sent: a running token count and the responses by id.
#[derive(Default)]
struct Session {
    sent_tokens: u64,
    entries: HashMap<String, Entry>,
    last_used: u64,
}

// A response already sent: its content hash, a description and the session's token count just after it.
struct Entry {
    hash: [u8; 32],
    desc: String,
    sent_at: u64,
}

// All sessions the daemon knows about.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<(HashMap<String, Session>, u64)>,
}

// Derives an id of the given length from a hash.
fn id_from(hash: &[u8; 32], len: usize) -> String {
    let mut bits = hash.iter().fold(0u128, |acc, b| (acc << 8) | u128::from(*b));
    (0..len)
        .map(|_| {
            let c = ALPHABET[(bits & 31) as usize] as char;
            bits >>= 5;
            c
        })
        .collect()
}

// Picks the shortest id for a response that no different response in the session already uses.
fn unique_id(entries: &HashMap<String, Entry>, hash: &[u8; 32]) -> String {
    (MIN_ID..=24)
        .map(|n| id_from(hash, n))
        .find(|id| entries.get(id).is_none_or(|e| e.hash == *hash))
        .expect("a 24-character id is always unique")
}

// Hashes a response together with its session.
fn hash_of(session: &str, text: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(session.as_bytes());
    h.update(&[0]);
    h.update(text.as_bytes());
    *h.finalize().as_bytes()
}

impl Sessions {
    // Returns the number of tracked sessions.
    pub fn count(&self) -> usize {
        self.inner.lock().expect("session lock").0.len()
    }

    // Tags a response with its id, or replaces it with a short notice if the session was already sent it recently.
    pub fn apply(&self, session: &str, desc: &str, text: String, allow_dedupe: bool) -> String {
        if session.is_empty() {
            return text;
        }
        let hash = hash_of(session, &text);
        let mut guard = self.inner.lock().expect("session lock");
        let (map, clock) = &mut *guard;
        *clock += 1;
        if !map.contains_key(session) && map.len() >= MAX_SESSIONS {
            let oldest =
                map.iter().min_by_key(|(_, s)| s.last_used).map(|(k, _)| k.clone()).expect("map is full");
            map.remove(&oldest);
        }
        let s = map.entry(session.to_string()).or_default();
        s.last_used = *clock;
        let id = unique_id(&s.entries, &hash);
        let seen = s.entries.get(&id).filter(|e| allow_dedupe && s.sent_tokens - e.sent_at < WINDOW_TOKENS);
        if let Some(e) = seen {
            return format!(
                "[r:{id}] already sent: {}, unchanged. Re-run with --disable-session-deduping to resend.",
                e.desc
            );
        }
        s.sent_tokens += tokens::estimate(&text) as u64;
        if s.entries.len() >= MAX_ENTRIES {
            let oldest = s
                .entries
                .iter()
                .min_by_key(|(_, e)| e.sent_at)
                .map(|(k, _)| k.clone())
                .expect("entries are full");
            s.entries.remove(&oldest);
        }
        s.entries.insert(id.clone(), Entry { hash, desc: desc.to_string(), sent_at: s.sent_tokens });
        format!("[r:{id}]\n{text}")
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks a first response is tagged and a repeat becomes a notice naming it.
    #[test]
    fn test_repeat_becomes_notice() {
        let s = Sessions::default();
        let first = s.apply("sess", "afo read a.rs", "content".into(), true);
        assert!(first.starts_with("[r:") && first.ends_with("]\ncontent"));
        let id = &first[3..8];
        let second = s.apply("sess", "afo read a.rs", "content".into(), true);
        assert_eq!(second, format!("[r:{id}] already sent: afo read a.rs, unchanged. Re-run with --disable-session-deduping to resend."));
    }

    // Checks the disable flag resends full text under the same id.
    #[test]
    fn test_disabled_dedupe_resends() {
        let s = Sessions::default();
        let first = s.apply("sess", "d", "content".into(), true);
        let again = s.apply("sess", "d", "content".into(), false);
        assert_eq!(first, again);
        assert!(s.apply("sess", "d", "content".into(), true).contains("already sent"));
    }

    // Checks different text and different sessions get independent ids and no notice.
    #[test]
    fn test_sessions_and_content_are_independent() {
        let s = Sessions::default();
        let a = s.apply("one", "d", "same".into(), true);
        let b = s.apply("two", "d", "same".into(), true);
        let c = s.apply("one", "d", "other".into(), true);
        assert!(a.ends_with("same") && b.ends_with("same") && c.ends_with("other"));
        assert_ne!(a[3..8], b[3..8]);
        assert_eq!(s.count(), 2);
    }

    // Checks no session means no ids and no dedupe.
    #[test]
    fn test_empty_session_is_untouched() {
        let s = Sessions::default();
        assert_eq!(s.apply("", "d", "text".into(), true), "text");
        assert_eq!(s.apply("", "d", "text".into(), true), "text");
        assert_eq!(s.count(), 0);
    }

    // Checks a response sent long ago is resent in full once the recency window has passed.
    #[test]
    fn test_window_expiry() {
        let s = Sessions::default();
        let big = "x".repeat(7 * WINDOW_TOKENS as usize / 2 + 100);
        s.apply("sess", "d", "first".into(), true);
        s.apply("sess", "big", big, true);
        let again = s.apply("sess", "d", "first".into(), true);
        assert!(again.ends_with("]\nfirst"), "{again}");
    }

    // Checks id extension when two different responses would share a short id.
    #[test]
    fn test_unique_id_extends_on_collision() {
        let mut entries = HashMap::new();
        let a = [1u8; 32];
        let mut b = a;
        b[28] ^= 0x02;
        entries.insert(id_from(&a, MIN_ID), Entry { hash: a, desc: String::new(), sent_at: 0 });
        assert_eq!(unique_id(&entries, &a), id_from(&a, MIN_ID));
        assert_eq!(id_from(&a, MIN_ID), id_from(&b, MIN_ID));
        assert_eq!(unique_id(&entries, &b).len(), MIN_ID + 1);
    }

    // Checks the least recently used session is dropped when too many are tracked.
    #[test]
    fn test_session_eviction() {
        let s = Sessions::default();
        for i in 0..MAX_SESSIONS + 3 {
            s.apply(&format!("s{i}"), "d", "t".into(), true);
        }
        assert_eq!(s.count(), MAX_SESSIONS);
        assert!(s.apply("s0", "d", "t".into(), true).starts_with("[r:"));
    }

    // Checks the oldest entry is dropped when a session holds too many responses.
    #[test]
    fn test_entry_eviction() {
        let s = Sessions::default();
        for i in 0..MAX_ENTRIES + 5 {
            s.apply("sess", "d", format!("text {i}"), true);
        }
        let (map, _) = &*s.inner.lock().expect("lock");
        assert_eq!(map["sess"].entries.len(), MAX_ENTRIES);
    }

    // Checks ids are five characters from the alphabet.
    #[test]
    fn test_id_shape() {
        let id = id_from(&[0xAB; 32], 5);
        assert_eq!(id.len(), 5);
        assert!(id.bytes().all(|b| ALPHABET.contains(&b)));
    }
}
