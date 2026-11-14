// The zazz.sig envelope: a canonical digest of every other entry, the signature over it and the signer's certificate chain.
use crate::manifest::SIGNATURE;
use crate::zip::{Archive, Entry};
use serde_json::{json, Value};

pub const DOMAIN: &[u8] = b"zazz-sig-v1\0";
pub const ALG: &str = "ecdsa-p256-sha256";
// Extended key usage for zazz code signing, under IANA's example enterprise number (RFC 5612); replace with a registered PEN.
pub const EKU_OID: &str = "1.3.6.1.4.1.32473.7979.1";
// The certificate extension holding an authority's grants as JSON, under the same arc.
pub const GRANTS_OID: &str = "1.3.6.1.4.1.32473.7979.2";

// A parsed zazz.sig.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub digest: String,
    pub sig: Vec<u8>,
    pub chain: Vec<Vec<u8>>,
}

// Hashes every entry except zazz.sig, in name order: name, length (u64 LE) and bytes.
pub fn digest(a: &Archive) -> String {
    let mut h = blake3::Hasher::new();
    for e in a.entries.iter().filter(|e| e.name != SIGNATURE) {
        h.update(e.name.as_bytes());
        h.update(&[0]);
        h.update(&(e.data.len() as u64).to_le_bytes());
        h.update(&e.data);
    }
    format!("b3:{}", h.finalize().to_hex())
}

// The bytes a signer signs: the domain string followed by the digest.
pub fn message(digest: &str) -> Vec<u8> {
    [DOMAIN, digest.as_bytes()].concat()
}

// Standard base64 with padding.
pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

// Decodes standard base64 with padding.
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err("base64 length is not a multiple of 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 {
            return Err("bad base64 padding".into());
        }
        let mut n = 0u32;
        for (i, b) in chunk.iter().enumerate() {
            let v = if i >= 4 - pad { 0 } else { val(*b).ok_or("bad base64 character")? };
            n = n << 6 | v as u32;
        }
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad]);
    }
    Ok(out)
}

impl Envelope {
    // Renders zazz.sig as JSON.
    pub fn to_json(&self) -> Value {
        json!({"v": 1, "alg": ALG, "digest": self.digest, "sig": b64(&self.sig), "chain": self.chain.iter().map(|c| b64(c)).collect::<Vec<_>>()})
    }

    // Reads zazz.sig, checking the version and algorithm.
    pub fn parse(text: &[u8]) -> Result<Envelope, String> {
        let v: Value = serde_json::from_slice(text).map_err(|e| format!("{SIGNATURE}: invalid JSON: {e}"))?;
        if v["v"] != 1 || v["alg"] != ALG {
            return Err(format!("{SIGNATURE}: unsupported version or algorithm (want v 1, {ALG})"));
        }
        let field = |k: &str| v[k].as_str().ok_or_else(|| format!("{SIGNATURE}: '{k}' is missing"));
        let chain: Result<Vec<Vec<u8>>, String> = v["chain"]
            .as_array()
            .ok_or(format!("{SIGNATURE}: 'chain' is missing"))?
            .iter()
            .map(|c| unb64(c.as_str().unwrap_or("?")))
            .collect();
        let chain = chain.map_err(|e| format!("{SIGNATURE}: chain: {e}"))?;
        if chain.is_empty() {
            return Err(format!("{SIGNATURE}: the chain needs the signer's certificate"));
        }
        Ok(Envelope {
            digest: field("digest")?.to_string(),
            sig: unb64(field("sig")?).map_err(|e| format!("{SIGNATURE}: sig: {e}"))?,
            chain,
        })
    }

    // Reads the envelope of an archive and checks its digest matches the archive's other entries.
    pub fn of(a: &Archive) -> Result<Option<Envelope>, String> {
        let Some(text) = a.get(SIGNATURE) else { return Ok(None) };
        let env = Envelope::parse(text)?;
        if env.digest != digest(a) {
            return Err(format!(
                "{SIGNATURE}: the digest does not match the archive (it was changed after signing)"
            ));
        }
        Ok(Some(env))
    }
}

// Returns a copy of the archive with zazz.sig set to the envelope.
pub fn with_envelope(a: &Archive, env: &Envelope) -> Archive {
    let mut entries: Vec<Entry> = a.entries.iter().filter(|e| e.name != SIGNATURE).cloned().collect();
    entries.push(Entry { name: SIGNATURE.into(), data: env.to_json().to_string().into_bytes() });
    Archive::new(entries)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Builds an archive from names and texts.
    fn arch(files: &[(&str, &str)]) -> Archive {
        Archive::new(
            files.iter().map(|(n, d)| Entry { name: n.to_string(), data: d.as_bytes().to_vec() }).collect(),
        )
    }

    // Checks the digest ignores zazz.sig and changes with any byte, name or boundary.
    #[test]
    fn test_digest() {
        let a = arch(&[("a", "1"), ("b", "2")]);
        let d = digest(&a);
        assert!(d.starts_with("b3:") && d.len() == 67);
        let signed = with_envelope(&a, &Envelope { digest: d.clone(), sig: vec![1], chain: vec![vec![2]] });
        assert_eq!(digest(&signed), d);
        assert_ne!(digest(&arch(&[("a", "1"), ("b", "3")])), d);
        assert_ne!(digest(&arch(&[("a", "12"), ("b", "")])), digest(&arch(&[("a", "1"), ("b", "2")])));
        assert_eq!(message("x"), b"zazz-sig-v1\0x");
    }

    // Checks base64 against known values and malformed input.
    #[test]
    fn test_base64() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(b64(raw.as_bytes()), enc);
            assert_eq!(unb64(enc).expect("decode"), raw.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(unb64(&b64(&all)).expect("all bytes"), all);
        assert!(unb64("abc").is_err() && unb64("a===").is_err() && unb64("ab!=").is_err());
    }

    // Checks the envelope round-trips, and every malformed field is reported.
    #[test]
    fn test_envelope() {
        let a = arch(&[("zazz.json", "{}")]);
        let env = Envelope { digest: digest(&a), sig: vec![9, 8], chain: vec![vec![1, 2, 3]] };
        let signed = with_envelope(&a, &env);
        assert_eq!(Envelope::of(&signed).expect("of"), Some(env.clone()));
        assert_eq!(Envelope::of(&a).expect("unsigned"), None);
        let tampered =
            Archive::new([signed.entries.clone(), vec![Entry { name: "x".into(), data: vec![] }]].concat());
        assert!(Envelope::of(&tampered).unwrap_err().contains("changed after signing"));
        let bad = |v: Value| Envelope::parse(v.to_string().as_bytes()).unwrap_err();
        assert!(bad(json!({"v": 2})).contains("unsupported"));
        assert!(bad(json!({"v": 1, "alg": ALG})).contains("'chain' is missing"));
        assert!(bad(json!({"v": 1, "alg": ALG, "chain": []})).contains("signer's certificate"));
        assert!(bad(json!({"v": 1, "alg": ALG, "chain": [1]})).contains("chain:"));
        assert!(bad(json!({"v": 1, "alg": ALG, "chain": ["AQ=="]})).contains("'digest' is missing"));
        assert!(
            bad(json!({"v": 1, "alg": ALG, "chain": ["AQ=="], "digest": "d", "sig": "!"})).contains("sig:")
        );
        assert!(Envelope::parse(b"{").unwrap_err().contains("invalid JSON"));
    }
}
