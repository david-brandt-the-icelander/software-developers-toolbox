// Signing keys and certificates for zazz authorities: P-256 keys, self-signed authority certificates, signatures and chain checks.
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{DerSignature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use serde_json::Value;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::asn1::{ObjectIdentifier, Utf8StringRef};
use x509_cert::der::{Decode, Encode, EncodePem};
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::Validity;
use x509_cert::Certificate;
use zazz_format::sig::{EKU_OID, GRANTS_OID};

const MAX_CHAIN: usize = 4;

// The grants extension: an authority's permissions as JSON text.
struct GrantsExt(String);

impl x509_cert::der::oid::AssociatedOid for GrantsExt {
    const OID: ObjectIdentifier = ObjectIdentifier::new_unwrap(GRANTS_OID);
}

impl Encode for GrantsExt {
    // The encoded length of the JSON as a UTF8String.
    fn encoded_len(&self) -> x509_cert::der::Result<x509_cert::der::Length> {
        Utf8StringRef::new(&self.0)?.encoded_len()
    }

    // Writes the JSON as a UTF8String.
    fn encode(&self, w: &mut impl x509_cert::der::Writer) -> x509_cert::der::Result<()> {
        Utf8StringRef::new(&self.0)?.encode(w)
    }
}

impl x509_cert::ext::AsExtension for GrantsExt {
    // Grants are not critical: other software may ignore them.
    fn critical(&self, _: &Name, _: &[x509_cert::ext::Extension]) -> bool {
        false
    }
}

// A new random signing key.
pub fn generate_key() -> SigningKey {
    SigningKey::random(&mut rand_core::OsRng)
}

// The key as PKCS#8 PEM.
pub fn key_pem(k: &SigningKey) -> Result<String, String> {
    k.to_pkcs8_pem(LineEnding::LF).map(|p| p.to_string()).map_err(|e| format!("encoding the key: {e}"))
}

// Reads a PKCS#8 PEM key.
pub fn key_from_pem(pem: &str) -> Result<SigningKey, String> {
    SigningKey::from_pkcs8_pem(pem).map_err(|e| format!("reading the key: {e}"))
}

// Builds a self-signed authority certificate carrying the zazz signing purpose and the grants.
pub fn self_signed(k: &SigningKey, name: &str, days: u64, grants: &Value) -> Result<Vec<u8>, String> {
    let e = |x: &dyn std::fmt::Display| format!("building the certificate: {x}");
    let subject = Name::from_str(&format!("CN={name},O=zazz authority")).map_err(|x| e(&x))?;
    let spki = SubjectPublicKeyInfoOwned::from_key(*k.verifying_key()).map_err(|x| e(&x))?;
    let serial = SerialNumber::new(&rand_serial()).map_err(|x| e(&x))?;
    let validity = Validity::from_now(Duration::from_secs(days * 86_400)).map_err(|x| e(&x))?;
    let mut b =
        CertificateBuilder::new(Profile::Root, serial, validity, subject, spki, k).map_err(|x| e(&x))?;
    let eku = ExtendedKeyUsage(vec![ObjectIdentifier::new_unwrap(EKU_OID)]);
    b.add_extension(&eku).map_err(|x| e(&x))?;
    b.add_extension(&GrantsExt(grants.to_string())).map_err(|x| e(&x))?;
    let cert = b.build::<DerSignature>().map_err(|x| e(&x))?;
    cert.to_der().map_err(|x| e(&x))
}

// Sixteen random bytes with the top bit clear, for a positive serial number.
fn rand_serial() -> [u8; 16] {
    let mut s = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut s);
    s[0] &= 0x7f;
    s
}

// A certificate as PEM text.
pub fn cert_pem(der: &[u8]) -> Result<String, String> {
    let c = Certificate::from_der(der).map_err(|e| e.to_string())?;
    c.to_pem(LineEnding::LF).map_err(|e| e.to_string())
}

// Reads every certificate in PEM text (or one DER certificate).
pub fn certs_from(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    if !data.starts_with(b"-----") {
        return Certificate::from_der(data)
            .map(|_| vec![data.to_vec()])
            .map_err(|e| format!("not a certificate: {e}"));
    }
    let certs = Certificate::load_pem_chain(data).map_err(|e| format!("not a PEM certificate: {e}"))?;
    certs.iter().map(|c| c.to_der().map_err(|e| e.to_string())).collect()
}

// Signs a message, returning a DER ECDSA signature.
pub fn sign(k: &SigningKey, msg: &[u8]) -> Vec<u8> {
    let sig: DerSignature = k.sign(msg);
    sig.as_bytes().to_vec()
}

// The P-256 key of a certificate.
fn key_of(c: &Certificate) -> Result<VerifyingKey, String> {
    let spki = c.tbs_certificate.subject_public_key_info.to_der().map_err(|e| e.to_string())?;
    p256::pkcs8::DecodePublicKey::from_public_key_der(&spki)
        .map_err(|_| "the certificate key is not P-256".to_string())
}

// Verifies a DER signature over a message with a certificate's key.
pub fn verify(cert_der: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), String> {
    let c = Certificate::from_der(cert_der).map_err(|e| format!("signer certificate: {e}"))?;
    let s = DerSignature::try_from(sig).map_err(|_| "the signature is not DER ECDSA".to_string())?;
    key_of(&c)?.verify(msg, &s).map_err(|_| "the signature does not match the signer's key".into())
}

// True when `child` was signed by `parent`'s key.
fn signed_by(child: &Certificate, parent: &Certificate) -> bool {
    let (Ok(tbs), Ok(key)) = (child.tbs_certificate.to_der(), key_of(parent)) else { return false };
    let Some(raw) = child.signature.as_bytes() else { return false };
    DerSignature::try_from(raw).is_ok_and(|s| key.verify(&tbs, &s).is_ok())
}

// Seconds since the Unix epoch.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// Checks a certificate is valid at a time.
fn in_validity(c: &Certificate, at: u64) -> bool {
    let v = &c.tbs_certificate.validity;
    v.not_before.to_unix_duration().as_secs() <= at && at <= v.not_after.to_unix_duration().as_secs()
}

// True when the certificate is a CA (basic constraints).
fn is_ca(c: &Certificate) -> bool {
    let id = <BasicConstraints as x509_cert::der::oid::AssociatedOid>::OID;
    extension(c, id).and_then(|v| BasicConstraints::from_der(&v).ok()).is_some_and(|b| b.ca)
}

// The raw value of an extension.
fn extension(c: &Certificate, id: ObjectIdentifier) -> Option<Vec<u8>> {
    let exts = c.tbs_certificate.extensions.as_ref()?;
    exts.iter().find(|e| e.extn_id == id).map(|e| e.extn_value.as_bytes().to_vec())
}

// True when the certificate allows zazz code signing.
fn has_zazz_eku(c: &Certificate) -> bool {
    let id = <ExtendedKeyUsage as x509_cert::der::oid::AssociatedOid>::OID;
    let want = ObjectIdentifier::new_unwrap(EKU_OID);
    extension(c, id).and_then(|v| ExtendedKeyUsage::from_der(&v).ok()).is_some_and(|e| e.0.contains(&want))
}

// The grants a certificate carries, if any.
pub fn grants_of(der: &[u8]) -> Option<Value> {
    let c = Certificate::from_der(der).ok()?;
    let v = extension(&c, ObjectIdentifier::new_unwrap(GRANTS_OID))?;
    serde_json::from_str(Utf8StringRef::from_der(&v).ok()?.as_str()).ok()
}

// The subject common name of a certificate, for messages.
pub fn subject(der: &[u8]) -> String {
    Certificate::from_der(der).map(|c| c.tbs_certificate.subject.to_string()).unwrap_or_else(|_| "?".into())
}

// Checks a chain [leaf, intermediates...] against trust anchors at a time; returns the index of the anchor that vouches for it.
pub fn verify_chain(chain: &[Vec<u8>], anchors: &[Vec<u8>], at: u64) -> Result<usize, String> {
    if chain.is_empty() || chain.len() > MAX_CHAIN {
        return Err(format!("the chain must hold 1 to {MAX_CHAIN} certificates"));
    }
    let certs: Vec<Certificate> = chain
        .iter()
        .map(|d| Certificate::from_der(d))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("chain: {e}"))?;
    if let Some(bad) = certs.iter().find(|c| !in_validity(c, at)) {
        return Err(format!("{} is expired or not yet valid", bad.tbs_certificate.subject));
    }
    if !has_zazz_eku(&certs[0]) {
        return Err("the signer certificate is not for zazz code signing".into());
    }
    for w in certs.windows(2) {
        if !is_ca(&w[1]) || !signed_by(&w[0], &w[1]) {
            return Err(format!(
                "{} is not signed by the CA {}",
                w[0].tbs_certificate.subject, w[1].tbs_certificate.subject
            ));
        }
    }
    let top = certs.last().expect("the chain is not empty");
    for (i, a) in anchors.iter().enumerate() {
        let Ok(anchor) = Certificate::from_der(a) else { continue };
        let same = a == chain.last().expect("the chain is not empty");
        let issued = top.tbs_certificate.issuer == anchor.tbs_certificate.subject
            && is_ca(&anchor)
            && signed_by(top, &anchor);
        if (same || issued) && in_validity(&anchor, at) {
            return Ok(i);
        }
    }
    Err(format!("no trusted authority vouches for {}", certs[0].tbs_certificate.subject))
}

// verify_chain at the current time.
pub fn verify_chain_now(chain: &[Vec<u8>], anchors: &[Vec<u8>]) -> Result<usize, String> {
    verify_chain(chain, anchors, now_secs())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub mod tests {
    use super::*;
    use serde_json::json;

    // Checks keys round-trip through PEM and signatures verify only with the right key and message.
    #[test]
    fn test_keys_and_signatures() {
        let k = generate_key();
        let pem = key_pem(&k).expect("pem");
        assert_eq!(key_from_pem(&pem).expect("read").verifying_key(), k.verifying_key());
        assert!(key_from_pem("junk").is_err());
        let cert = self_signed(&k, "alice", 30, &json!({"native": true})).expect("cert");
        let sig = sign(&k, b"msg");
        assert!(verify(&cert, b"msg", &sig).is_ok());
        assert!(verify(&cert, b"other", &sig).unwrap_err().contains("does not match"));
        assert!(verify(&cert, b"msg", b"junk").unwrap_err().contains("not DER"));
        assert!(verify(b"junk", b"msg", &sig).is_err());
        assert_eq!(grants_of(&cert), Some(json!({"native": true})));
        assert!(subject(&cert).contains("CN=alice"));
        assert_eq!(subject(b"x"), "?");
        assert_eq!(grants_of(b"x"), None);
    }

    // Checks certificates round-trip through PEM, and chains of PEM certificates load.
    #[test]
    fn test_pem() {
        let k = generate_key();
        let cert = self_signed(&k, "a", 1, &json!({})).expect("cert");
        let pem = cert_pem(&cert).expect("pem");
        assert_eq!(certs_from(pem.as_bytes()).expect("pem"), vec![cert.clone()]);
        assert_eq!(certs_from(&cert).expect("der"), vec![cert.clone()]);
        let two = format!("{pem}{pem}");
        assert_eq!(certs_from(two.as_bytes()).expect("two").len(), 2);
        assert!(certs_from(b"junk").is_err() && certs_from(b"-----BEGIN junk").is_err());
        assert!(cert_pem(b"junk").is_err());
    }

    // Checks chains: a self-signed authority vouches for itself; unknown, expired or unrelated chains fail.
    #[test]
    fn test_verify_chain() {
        let (a, b) = (generate_key(), generate_key());
        let ca = self_signed(&a, "a", 30, &json!({})).expect("a");
        let other = self_signed(&b, "b", 30, &json!({})).expect("b");
        assert_eq!(
            verify_chain_now(std::slice::from_ref(&ca), &[other.clone(), ca.clone()]).expect("trusted"),
            1
        );
        assert!(verify_chain_now(std::slice::from_ref(&ca), std::slice::from_ref(&other))
            .unwrap_err()
            .contains("no trusted authority"));
        assert!(verify_chain(std::slice::from_ref(&ca), std::slice::from_ref(&ca), 0)
            .unwrap_err()
            .contains("expired"));
        assert!(verify_chain_now(&[], &[]).is_err());
        assert!(verify_chain_now(&[b"junk".to_vec()], &[]).unwrap_err().starts_with("chain:"));
        assert!(verify_chain_now(&[ca.clone(), other.clone()], std::slice::from_ref(&other))
            .unwrap_err()
            .contains("is not signed by"));
        assert!(verify_chain_now(std::slice::from_ref(&ca), &[b"junk".to_vec(), ca.clone()]).is_ok());
        let too_long = vec![ca.clone(); MAX_CHAIN + 1];
        assert!(verify_chain_now(&too_long, &[]).is_err());
    }

    // Issues a leaf certificate for zazz signing from a CA key.
    pub fn leaf_from(ca_key: &SigningKey, ca_der: &[u8], leaf_key: &SigningKey, grants: &Value) -> Vec<u8> {
        let ca = Certificate::from_der(ca_der).expect("ca");
        let profile = Profile::Leaf {
            issuer: ca.tbs_certificate.subject.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: false,
        };
        let subject = Name::from_str("CN=leaf").expect("name");
        let spki = SubjectPublicKeyInfoOwned::from_key(*leaf_key.verifying_key()).expect("spki");
        let validity = Validity::from_now(Duration::from_secs(86_400)).expect("validity");
        let mut b =
            CertificateBuilder::new(profile, SerialNumber::from(9u32), validity, subject, spki, ca_key)
                .expect("builder");
        b.add_extension(&ExtendedKeyUsage(vec![ObjectIdentifier::new_unwrap(EKU_OID)])).expect("eku");
        b.add_extension(&GrantsExt(grants.to_string())).expect("grants");
        b.build::<DerSignature>().expect("leaf").to_der().expect("der")
    }

    // Checks a leaf issued by a CA: trusted through the CA as anchor or as the chain's top, and refused when the parent is no CA.
    #[test]
    fn test_chain_with_issuer() {
        let (ca_key, leaf_key, stray) = (generate_key(), generate_key(), generate_key());
        let ca = self_signed(&ca_key, "org", 30, &json!({"native": true})).expect("ca");
        let leaf = leaf_from(&ca_key, &ca, &leaf_key, &json!({"native": false}));
        assert_eq!(
            verify_chain_now(std::slice::from_ref(&leaf), std::slice::from_ref(&ca)).expect("issued"),
            0
        );
        assert_eq!(
            verify_chain_now(&[leaf.clone(), ca.clone()], std::slice::from_ref(&ca)).expect("same top"),
            0
        );
        let other_leaf = leaf_from(&ca_key, &ca, &stray, &json!({}));
        assert!(verify_chain_now(&[leaf.clone(), other_leaf], std::slice::from_ref(&ca))
            .unwrap_err()
            .contains("is not signed by"));
        let under_leaf = leaf_from(&leaf_key, &leaf, &stray, &json!({}));
        assert!(verify_chain_now(std::slice::from_ref(&under_leaf), std::slice::from_ref(&leaf))
            .unwrap_err()
            .contains("no trusted authority"));
        let forged = leaf_from(&stray, &ca, &leaf_key, &json!({}));
        assert!(verify_chain_now(std::slice::from_ref(&forged), std::slice::from_ref(&ca)).is_err());
        assert!(!signed_by(
            &Certificate::from_der(&leaf).expect("leaf"),
            &Certificate::from_der(&leaf).expect("leaf")
        ));
    }

    // Checks a certificate without the zazz purpose cannot sign scripts.
    #[test]
    fn test_requires_zazz_eku() {
        let k = generate_key();
        let subject = Name::from_str("CN=plain").expect("name");
        let spki = SubjectPublicKeyInfoOwned::from_key(*k.verifying_key()).expect("spki");
        let validity = Validity::from_now(Duration::from_secs(86_400)).expect("validity");
        let b = CertificateBuilder::new(Profile::Root, SerialNumber::from(7u32), validity, subject, spki, &k)
            .expect("builder");
        let plain = b.build::<DerSignature>().expect("cert").to_der().expect("der");
        assert!(verify_chain_now(std::slice::from_ref(&plain), std::slice::from_ref(&plain))
            .unwrap_err()
            .contains("not for zazz"));
        assert_eq!(grants_of(&plain), None);
    }
}
