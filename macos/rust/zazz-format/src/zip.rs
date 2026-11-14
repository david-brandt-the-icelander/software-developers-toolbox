// ZIP reading and writing for .zazz files: stored and deflate only, strict limits, deterministic output.
use std::collections::HashSet;

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD_LEN: usize = 22;
const LOCAL_LEN: usize = 30;
const CENTRAL_LEN: usize = 46;
const MAX_COMMENT: usize = 0xffff;
const FLAG_ENCRYPTED: u16 = 1;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;
const STORED: u16 = 0;
const DEFLATE: u16 = 8;
const DOS_DATE_1980: u16 = 0x0021;
const VERSION: u16 = 20;
const MADE_BY_UNIX: u16 = 0x031e;
const FILE_MODE: u32 = 0o100_644;
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const MAX_NAME: usize = 1024;
const MAX_COMPONENT: usize = 255;
const RATIO_FLOOR: u64 = 64 * 1024;

// Size limits applied before anything is decompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub compressed: u64,
    pub uncompressed: u64,
    pub entries: usize,
    pub entry: u64,
    pub ratio: u64,
}

impl Limits {
    pub const DEFAULT: Limits =
        Limits { compressed: 32 << 20, uncompressed: 128 << 20, entries: 1024, entry: 64 << 20, ratio: 100 };
}

// One file in an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

// A validated, fully decompressed archive with entries sorted by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub entries: Vec<Entry>,
}

impl Archive {
    // Builds an archive from entries, sorting them by name.
    pub fn new(mut entries: Vec<Entry>) -> Archive {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Archive { entries }
    }

    // Returns the bytes of the named entry.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let i = self.entries.binary_search_by(|e| e.name.as_str().cmp(name)).ok()?;
        Some(&self.entries[i].data)
    }

    // Returns the entries below a directory, given without a trailing slash.
    pub fn under<'a>(&'a self, dir: &'a str) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries.iter().filter(move |e| e.name.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
    }
}

// A central directory record, as far as validation needs it.
struct Central {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    local: u64,
    dir: bool,
}

// Reads a little-endian u16 at an offset, failing on truncation.
fn u16_at(b: &[u8], off: usize) -> Result<u16, String> {
    b.get(off..off + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| "archive is truncated".into())
}

// Reads a little-endian u32 at an offset, failing on truncation.
fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "archive is truncated".into())
}

// Computes the CRC-32 (IEEE) of a byte slice.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 == 1 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, b| table[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8))
}

// Checks that an entry name is a safe relative path that extracts to the same place everywhere.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("entry name '{name}' is empty or longer than {MAX_NAME} bytes"));
    }
    if name.starts_with('/') || name.contains(['\\', ':']) || name.chars().any(char::is_control) {
        return Err(format!("entry name '{name}' must be relative and use only '/' as a separator"));
    }
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.len() > MAX_COMPONENT {
            return Err(format!("entry name '{name}' has an empty, '.', '..' or overlong component"));
        }
    }
    Ok(())
}

// Finds the end-of-central-directory record, which must end exactly at the end of the file.
fn find_eocd(b: &[u8]) -> Result<usize, String> {
    if b.len() < EOCD_LEN {
        return Err("not a zip archive (too short)".into());
    }
    let lowest = b.len().saturating_sub(EOCD_LEN + MAX_COMMENT);
    for pos in (lowest..=b.len() - EOCD_LEN).rev() {
        if u32_at(b, pos)? == EOCD_SIG && pos + EOCD_LEN + u16_at(b, pos + 20)? as usize == b.len() {
            return Ok(pos);
        }
    }
    Err("not a zip archive (no end record)".into())
}

// Decodes the Unix file type from a central record, if the archive was made on Unix.
fn unix_kind(made_by: u16, attrs: u32) -> Option<u32> {
    (made_by >> 8 == 3 && attrs >> 16 != 0).then_some((attrs >> 16) & S_IFMT)
}

// Parses and validates one central directory record at an offset, returning it and the next offset.
fn central_at(b: &[u8], p: usize) -> Result<(Central, usize), String> {
    if u32_at(b, p)? != CENTRAL_SIG {
        return Err("corrupt central directory".into());
    }
    let (made_by, flags, method) = (u16_at(b, p + 4)?, u16_at(b, p + 8)?, u16_at(b, p + 10)?);
    let (crc, csize, usize) = (u32_at(b, p + 16)?, u32_at(b, p + 20)?, u32_at(b, p + 24)?);
    let (nlen, xlen, clen) =
        (u16_at(b, p + 28)? as usize, u16_at(b, p + 30)? as usize, u16_at(b, p + 32)? as usize);
    let (disk, attrs, local) = (u16_at(b, p + 34)?, u32_at(b, p + 38)?, u32_at(b, p + 42)?);
    let raw = b.get(p + CENTRAL_LEN..p + CENTRAL_LEN + nlen).ok_or("archive is truncated")?;
    let name = String::from_utf8(raw.to_vec()).map_err(|_| "entry name is not UTF-8".to_string())?;
    if csize == u32::MAX || usize == u32::MAX || local == u32::MAX || disk != 0 {
        return Err(format!("{name}: zip64 and multi-disk archives are not supported"));
    }
    if flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION) != 0 {
        return Err(format!("{name}: encrypted entries are not supported"));
    }
    if method != STORED && method != DEFLATE {
        return Err(format!("{name}: compression method {method} is not supported (use stored or deflate)"));
    }
    let dir = name.ends_with('/');
    check_name(name.strip_suffix('/').unwrap_or(&name))?;
    match unix_kind(made_by, attrs) {
        Some(S_IFDIR) if dir => {}
        Some(S_IFREG) if !dir => {}
        None => {}
        Some(_) => return Err(format!("{name}: symlinks and special files are not allowed")),
    }
    if dir && usize != 0 {
        return Err(format!("{name}: a directory entry has data"));
    }
    let next = p + CENTRAL_LEN + nlen + xlen + clen;
    let c = Central {
        name,
        flags,
        method,
        crc,
        csize: csize as u64,
        usize: usize as u64,
        local: local as u64,
        dir,
    };
    Ok((c, next))
}

// Returns where an entry's compressed data starts, after checking its local header agrees with the central one.
fn data_start(b: &[u8], c: &Central) -> Result<usize, String> {
    let p = c.local as usize;
    if u32_at(b, p)? != LOCAL_SIG {
        return Err(format!("{}: missing local header", c.name));
    }
    let (flags, method) = (u16_at(b, p + 6)?, u16_at(b, p + 8)?);
    let (nlen, xlen) = (u16_at(b, p + 26)? as usize, u16_at(b, p + 28)? as usize);
    let name = b.get(p + LOCAL_LEN..p + LOCAL_LEN + nlen).ok_or("archive is truncated")?;
    if name != c.name.as_bytes() || method != c.method || flags != c.flags {
        return Err(format!("{}: local header disagrees with the central directory", c.name));
    }
    let sizes = (u32_at(b, p + 14)?, u32_at(b, p + 18)? as u64, u32_at(b, p + 22)? as u64);
    if flags & FLAG_DESCRIPTOR == 0 && sizes != (c.crc, c.csize, c.usize) {
        return Err(format!("{}: local header sizes disagree with the central directory", c.name));
    }
    Ok(p + LOCAL_LEN + nlen + xlen)
}

// Rejects totals, per-entry sizes and compression ratios over the limits, before any inflating.
fn check_sizes(files: &[&Central], limits: &Limits) -> Result<(), String> {
    let mut total = 0u64;
    for c in files {
        if c.usize > limits.entry {
            return Err(format!(
                "{}: {} bytes is over the per-entry limit of {}",
                c.name, c.usize, limits.entry
            ));
        }
        if c.usize > (c.csize.saturating_mul(limits.ratio)).max(RATIO_FLOOR) {
            return Err(format!("{}: compression ratio is over {}:1", c.name, limits.ratio));
        }
        total += c.usize;
    }
    if total > limits.uncompressed {
        return Err(format!("archive unpacks to {total} bytes; the limit is {}", limits.uncompressed));
    }
    Ok(())
}

// Rejects names that collide after case folding, or a file that is also another file's directory.
fn check_collisions(names: &[&str]) -> Result<(), String> {
    let folded: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
    if folded.len() != names.len() {
        return Err("two entries have the same name (ignoring case)".into());
    }
    for n in names {
        let mut prefix = String::new();
        for part in n.split('/').take(n.split('/').count() - 1) {
            prefix.push_str(part);
            if folded.contains(&prefix.to_lowercase()) {
                return Err(format!("'{prefix}' is both a file and a directory"));
            }
            prefix.push('/');
        }
    }
    Ok(())
}

// Inflates or copies one entry and checks its length and CRC.
fn extract(b: &[u8], c: &Central, start: usize) -> Result<Vec<u8>, String> {
    let raw = &b[start..start + c.csize as usize];
    let data = if c.method == STORED {
        raw.to_vec()
    } else {
        miniz_oxide::inflate::decompress_to_vec_with_limit(raw, c.usize as usize)
            .map_err(|_| format!("{}: corrupt or oversized deflate data", c.name))?
    };
    if data.len() as u64 != c.usize || crc32(&data) != c.crc {
        return Err(format!("{}: size or CRC mismatch", c.name));
    }
    Ok(data)
}

// Reads the central directory records, which must fill the space before the end record exactly.
fn read_central(b: &[u8], eocd: usize, limits: &Limits) -> Result<(Vec<Central>, usize), String> {
    let (disk, cd_disk) = (u16_at(b, eocd + 4)?, u16_at(b, eocd + 6)?);
    let (here, count) = (u16_at(b, eocd + 8)?, u16_at(b, eocd + 10)? as usize);
    let (size, off) = (u32_at(b, eocd + 12)? as usize, u32_at(b, eocd + 16)? as usize);
    if disk != 0 || cd_disk != 0 || here as usize != count || count == 0xffff || off as u32 == u32::MAX {
        return Err("zip64 and multi-disk archives are not supported".into());
    }
    if count > limits.entries {
        return Err(format!("archive has {count} entries; the limit is {}", limits.entries));
    }
    if off.checked_add(size) != Some(eocd) {
        return Err("central directory does not end at the end record".into());
    }
    let (mut out, mut p) = (Vec::with_capacity(count), off);
    for _ in 0..count {
        let (c, next) = central_at(b, p)?;
        out.push(c);
        p = next;
    }
    if p != eocd {
        return Err("central directory size is wrong".into());
    }
    Ok((out, off))
}

// Parses, validates and decompresses a .zazz archive held in memory.
pub fn read(bytes: &[u8], limits: &Limits) -> Result<Archive, String> {
    if bytes.len() as u64 > limits.compressed {
        return Err(format!("archive is {} bytes; the limit is {}", bytes.len(), limits.compressed));
    }
    let eocd = find_eocd(bytes)?;
    let (central, cd_start) = read_central(bytes, eocd, limits)?;
    let files: Vec<&Central> = central.iter().filter(|c| !c.dir).collect();
    check_sizes(&files, limits)?;
    check_collisions(&files.iter().map(|c| c.name.as_str()).collect::<Vec<_>>())?;
    let mut spans = Vec::with_capacity(files.len());
    for c in &files {
        let start = data_start(bytes, c)?;
        let end = start.checked_add(c.csize as usize).filter(|e| *e <= cd_start);
        let end = end.ok_or_else(|| format!("{}: data runs past the central directory", c.name))?;
        spans.push((c.local as usize, end, start));
    }
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|i| spans[*i].0);
    if order.windows(2).any(|w| spans[w[1]].0 < spans[w[0]].1) {
        return Err("entries overlap (zip bomb)".into());
    }
    let mut entries = Vec::with_capacity(files.len());
    for (c, span) in files.iter().zip(&spans) {
        entries.push(Entry { name: c.name.clone(), data: extract(bytes, c, span.2)? });
    }
    Ok(Archive::new(entries))
}

// Appends a little-endian u16.
fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

// Appends a little-endian u32.
fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

// Writes the header fields shared by local and central records, from "version needed" to the name length.
fn put_common(out: &mut Vec<u8>, method: u16, crc: u32, csize: u32, usize: u32, nlen: u16) {
    for v in [VERSION, FLAG_UTF8, method, 0, DOS_DATE_1980] {
        put16(out, v);
    }
    for v in [crc, csize, usize] {
        put32(out, v);
    }
    put16(out, nlen);
    put16(out, 0);
}

// Serialises an archive deterministically: sorted names, fixed dates and modes, deflate level 9 when smaller.
pub fn write(archive: &Archive) -> Result<Vec<u8>, String> {
    if archive.entries.len() >= 0xffff {
        return Err("too many entries for a zip archive".into());
    }
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for e in &archive.entries {
        check_name(&e.name)?;
        let packed = miniz_oxide::deflate::compress_to_vec(&e.data, 9);
        let (method, body) = if packed.len() < e.data.len() { (DEFLATE, &packed) } else { (STORED, &e.data) };
        let too_big = |n: usize| u32::try_from(n).map_err(|_| format!("{}: entry is too large", e.name));
        let (crc, csize, usize, local) =
            (crc32(&e.data), too_big(body.len())?, too_big(e.data.len())?, too_big(out.len())?);
        let nlen = e.name.len() as u16;
        put32(&mut out, LOCAL_SIG);
        put_common(&mut out, method, crc, csize, usize, nlen);
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(body);
        put32(&mut central, CENTRAL_SIG);
        put16(&mut central, MADE_BY_UNIX);
        put_common(&mut central, method, crc, csize, usize, nlen);
        for v in [0u16, 0, 0] {
            put16(&mut central, v);
        }
        put32(&mut central, FILE_MODE << 16);
        put32(&mut central, local);
        central.extend_from_slice(e.name.as_bytes());
    }
    let off = u32::try_from(out.len()).map_err(|_| "archive is too large".to_string())?;
    out.extend_from_slice(&central);
    put32(&mut out, EOCD_SIG);
    for v in [0u16, 0, archive.entries.len() as u16, archive.entries.len() as u16] {
        put16(&mut out, v);
    }
    put32(&mut out, central.len() as u32);
    put32(&mut out, off);
    put16(&mut out, 0);
    Ok(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Builds an archive from name and text pairs.
    fn arch(files: &[(&str, &str)]) -> Archive {
        Archive::new(
            files.iter().map(|(n, d)| Entry { name: n.to_string(), data: d.as_bytes().to_vec() }).collect(),
        )
    }

    // Finds the offset of the first central record in a written archive.
    fn cd_offset(z: &[u8]) -> usize {
        let eocd = find_eocd(z).expect("eocd");
        u32_at(z, eocd + 16).expect("off") as usize
    }

    // Checks the CRC-32 of a known string.
    #[test]
    fn test_crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    // Checks that write then read returns the same entries, compressed and stored alike.
    #[test]
    fn test_write_read_roundtrip() {
        let a = arch(&[("zazz.json", "{}"), ("src/main.rs", &"fn main() {}\n".repeat(50)), ("x", "")]);
        let z = write(&a).expect("write");
        let back = read(&z, &Limits::DEFAULT).expect("read");
        assert_eq!(back, a);
        assert_eq!(back.get("x"), Some(&b""[..]));
        assert_eq!(back.get("nope"), None);
        assert_eq!(back.under("src").count(), 1);
        assert_eq!(back.under("sr").count(), 0);
    }

    // Checks that output is byte-for-byte deterministic regardless of input order.
    #[test]
    fn test_write_is_deterministic() {
        let a = write(&arch(&[("b", "2"), ("a", "1")])).expect("a");
        let b = write(&arch(&[("a", "1"), ("b", "2")])).expect("b");
        assert_eq!(a, b);
    }

    // Checks that unsafe or odd names are refused.
    #[test]
    fn test_check_name_rejects_unsafe_names() {
        for bad in [
            "",
            "/abs",
            "a/../b",
            "..",
            "./a",
            "a//b",
            "a\\b",
            "c:x",
            "a\nb",
            &"x".repeat(256),
            &"a/".repeat(600),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} should be refused");
        }
        for good in ["a", "src/main.rs", "tests/fixtures/a b.txt", "ünïcode"] {
            assert!(check_name(good).is_ok(), "{good:?} should pass");
        }
    }

    // Checks that writing refuses a bad name.
    #[test]
    fn test_write_rejects_bad_name() {
        assert!(write(&arch(&[("../x", "1")])).is_err());
    }

    // Checks that garbage, truncation and missing records are reported, not panics.
    #[test]
    fn test_read_rejects_garbage() {
        assert_eq!(read(b"short", &Limits::DEFAULT).unwrap_err(), "not a zip archive (too short)");
        assert_eq!(read(&[0u8; 100], &Limits::DEFAULT).unwrap_err(), "not a zip archive (no end record)");
        let z = write(&arch(&[("a", "hello")])).expect("write");
        for cut in 1..z.len() {
            assert!(read(&z[..cut], &Limits::DEFAULT).is_err());
        }
        let mut trailing = z.clone();
        trailing.push(0);
        assert!(read(&trailing, &Limits::DEFAULT).is_err());
    }

    // Checks the size, count, per-entry and ratio limits.
    #[test]
    fn test_read_enforces_limits() {
        let z = write(&arch(&[("a", &"a".repeat(200_000)), ("b", "x")])).expect("write");
        let lim = |f: fn(&mut Limits)| {
            let mut l = Limits::DEFAULT;
            f(&mut l);
            read(&z, &l).unwrap_err()
        };
        assert!(lim(|l| l.compressed = 10).contains("limit is 10"));
        assert!(lim(|l| l.entries = 1).contains("2 entries"));
        assert!(lim(|l| l.entry = 1000).contains("per-entry limit"));
        assert!(lim(|l| l.ratio = 2).contains("compression ratio"));
        assert!(lim(|l| {
            l.uncompressed = 1000;
            l.ratio = 1_000_000;
        })
        .contains("unpacks to"));
    }

    // Checks that case-folded duplicates and file/directory clashes are refused.
    #[test]
    fn test_read_rejects_collisions() {
        let z = write(&arch(&[("A.txt", "1"), ("a.txt", "2")])).expect("write");
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("same name"));
        let z = write(&arch(&[("src", "1"), ("src/a", "2")])).expect("write");
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("both a file and a directory"));
    }

    // Patches a u16 in every central record and returns the archive.
    fn patch_central16(mut z: Vec<u8>, field: usize, v: u16) -> Vec<u8> {
        let p = cd_offset(&z);
        z[p + field..p + field + 2].copy_from_slice(&v.to_le_bytes());
        z
    }

    // Patches a u32 in the first central record and returns the archive.
    fn patch_central32(mut z: Vec<u8>, field: usize, v: u32) -> Vec<u8> {
        let p = cd_offset(&z);
        z[p + field..p + field + 4].copy_from_slice(&v.to_le_bytes());
        z
    }

    // Checks encryption, unknown methods, zip64 markers, symlinks and non-UTF-8 names are refused.
    #[test]
    fn test_read_rejects_unsupported_entries() {
        let z = write(&arch(&[("a", "hello")])).expect("write");
        let err = |z: Vec<u8>| read(&z, &Limits::DEFAULT).unwrap_err();
        assert!(err(patch_central16(z.clone(), 8, FLAG_UTF8 | 1)).contains("encrypted"));
        assert!(err(patch_central16(z.clone(), 10, 12)).contains("method 12"));
        assert!(err(patch_central32(z.clone(), 20, u32::MAX)).contains("zip64"));
        assert!(err(patch_central32(z.clone(), 24, u32::MAX)).contains("zip64"));
        assert!(err(patch_central32(z.clone(), 42, u32::MAX)).contains("zip64"));
        assert!(read(&patch_central16(z.clone(), 4, 20), &Limits::DEFAULT).is_ok());
        assert!(err(patch_central16(z.clone(), 34, 1)).contains("multi-disk"));
        assert!(err(patch_central32(z.clone(), 38, 0o120_777 << 16)).contains("symlinks"));
        let mut bad_name = z.clone();
        let p = cd_offset(&bad_name) + CENTRAL_LEN;
        bad_name[p] = 0xff;
        assert!(err(bad_name).contains("not UTF-8"));
        assert!(err(patch_central32(z.clone(), 0, 0)).contains("corrupt central"));
    }

    // Checks local headers that disagree with the central directory are refused.
    #[test]
    fn test_read_rejects_local_mismatch() {
        let z = write(&arch(&[("a", "hello")])).expect("write");
        let err = |z: Vec<u8>| read(&z, &Limits::DEFAULT).unwrap_err();
        let mut m = z.clone();
        m[0] = 0;
        assert!(err(m).contains("missing local header"));
        let mut m = z.clone();
        m[LOCAL_LEN] = b'b';
        assert!(err(m).contains("disagrees"));
        let mut m = z.clone();
        m[18] ^= 1;
        assert!(err(m).contains("sizes disagree"));
        let mut m = z.clone();
        m[8] = 8;
        assert!(err(m).contains("disagrees"));
        let mut m = z.clone();
        m[7] = 0;
        assert!(err(m).contains("disagrees"));
    }

    // Checks a data descriptor flag lets the local sizes be zero, as streaming writers produce.
    #[test]
    fn test_read_accepts_data_descriptor() {
        let mut z = write(&arch(&[("a", "hello")])).expect("write");
        let flags = FLAG_UTF8 | FLAG_DESCRIPTOR;
        z[6..8].copy_from_slice(&flags.to_le_bytes());
        z[14..26].fill(0);
        let z = patch_central16(z, 8, flags);
        assert_eq!(read(&z, &Limits::DEFAULT).expect("read").get("a"), Some(&b"hello"[..]));
    }

    // Checks directory entries are skipped, but directories with data or odd kinds are refused.
    #[test]
    fn test_read_directory_entries() {
        let z = write_raw(&[("d/", b"", S_IFDIR | 0o755), ("d/f", b"x", FILE_MODE)]);
        assert_eq!(read(&z, &Limits::DEFAULT).expect("read").entries.len(), 1);
        let z = write_raw(&[("d/", b"x", S_IFDIR | 0o755)]);
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("directory entry has data"));
        let z = write_raw(&[("f", b"x", S_IFDIR | 0o755)]);
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("special files"));
        let z = write_raw(&[("d/", b"", FILE_MODE)]);
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("special files"));
        let z = write_raw(&[("f", b"x", 0)]);
        assert!(read(&z, &Limits::DEFAULT).is_ok());
    }

    // Writes stored entries with explicit Unix modes, bypassing name checks for directory names.
    fn write_raw(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data, mode) in files {
            let (crc, n, local) = (crc32(data), data.len() as u32, out.len() as u32);
            put32(&mut out, LOCAL_SIG);
            put_common(&mut out, STORED, crc, n, n, name.len() as u16);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            put32(&mut central, CENTRAL_SIG);
            put16(&mut central, MADE_BY_UNIX);
            put_common(&mut central, STORED, crc, n, n, name.len() as u16);
            for v in [0u16, 0, 0] {
                put16(&mut central, v);
            }
            put32(&mut central, mode << 16);
            put32(&mut central, local);
            central.extend_from_slice(name.as_bytes());
        }
        let off = out.len() as u32;
        out.extend_from_slice(&central);
        put32(&mut out, EOCD_SIG);
        for v in [0u16, 0, files.len() as u16, files.len() as u16] {
            put16(&mut out, v);
        }
        put32(&mut out, central.len() as u32);
        put32(&mut out, off);
        put16(&mut out, 0);
        out
    }

    // Checks overlapping entries (the classic non-recursive zip bomb) are refused.
    #[test]
    fn test_read_rejects_overlap() {
        let z = write_raw(&[("a", b"hello", FILE_MODE), ("b", b"world", FILE_MODE)]);
        let p = cd_offset(&z);
        let second = p + CENTRAL_LEN + 1;
        let mut m = z.clone();
        m[second + 42..second + 46].copy_from_slice(&0u32.to_le_bytes());
        let err = read(&m, &Limits::DEFAULT).unwrap_err();
        assert!(err.contains("overlap") || err.contains("disagrees"), "{err}");
    }

    // Checks a stored entry whose CRC is wrong in both headers is caught after extraction.
    #[test]
    fn test_read_rejects_wrong_crc() {
        let z = write_raw(&[("a", b"hello", FILE_MODE)]);
        let mut m = patch_central32(z, 16, 7);
        m[14..18].copy_from_slice(&7u32.to_le_bytes());
        assert!(read(&m, &Limits::DEFAULT).unwrap_err().contains("CRC mismatch"));
        let z = write_raw(&[("a", b"hello", FILE_MODE)]);
        let mut m = patch_central32(z, 24, 6);
        m[22..26].copy_from_slice(&6u32.to_le_bytes());
        assert!(read(&m, &Limits::DEFAULT).unwrap_err().contains("size or CRC mismatch"));
    }

    // Checks an entry whose data hides another entry's local header is refused as an overlap.
    #[test]
    fn test_read_rejects_nested_entry() {
        let mut inner = Vec::new();
        put32(&mut inner, LOCAL_SIG);
        put_common(&mut inner, STORED, crc32(b"x"), 1, 1, 1);
        inner.extend_from_slice(b"bx");
        let mut z = write_raw(&[("a", &inner, FILE_MODE)]);
        let eocd = find_eocd(&z).expect("eocd");
        let off = u32_at(&z, eocd + 16).expect("off") as usize;
        let mut central = z[off..eocd].to_vec();
        put32(&mut central, CENTRAL_SIG);
        put16(&mut central, MADE_BY_UNIX);
        put_common(&mut central, STORED, crc32(b"x"), 1, 1, 1);
        for v in [0u16, 0, 0] {
            put16(&mut central, v);
        }
        put32(&mut central, FILE_MODE << 16);
        put32(&mut central, (LOCAL_LEN + 1) as u32);
        central.push(b'b');
        z.truncate(off);
        z.extend_from_slice(&central);
        put32(&mut z, EOCD_SIG);
        for v in [0u16, 0, 2, 2] {
            put16(&mut z, v);
        }
        put32(&mut z, central.len() as u32);
        put32(&mut z, off as u32);
        put16(&mut z, 0);
        assert!(read(&z, &Limits::DEFAULT).unwrap_err().contains("overlap"));
    }

    // Checks a corrupted deflate stream and a wrong CRC are reported.
    #[test]
    fn test_read_rejects_corrupt_data() {
        let z = write(&arch(&[("a", &"abc".repeat(1000))])).expect("write");
        let mut m = z.clone();
        m[LOCAL_LEN + 1] ^= 0xff;
        m[LOCAL_LEN + 2] ^= 0xff;
        assert!(read(&m, &Limits::DEFAULT).is_err());
        let mut m = patch_central32(z.clone(), 16, 1);
        m[14..18].copy_from_slice(&1u32.to_le_bytes());
        assert!(read(&m, &Limits::DEFAULT).unwrap_err().contains("mismatch"));
    }

    // Checks end-record inconsistencies are refused.
    #[test]
    fn test_read_rejects_bad_end_record() {
        let z = write(&arch(&[("a", "hello")])).expect("write");
        let eocd = find_eocd(&z).expect("eocd");
        let patch = |off: usize, v: &[u8]| {
            let mut m = z.clone();
            m[eocd + off..eocd + off + v.len()].copy_from_slice(v);
            read(&m, &Limits::DEFAULT).unwrap_err()
        };
        assert!(patch(4, &1u16.to_le_bytes()).contains("multi-disk"));
        assert!(patch(8, &2u16.to_le_bytes()).contains("multi-disk"));
        assert!(patch(12, &1u32.to_le_bytes()).contains("does not end"));
        assert!(patch(16, &u32::MAX.to_le_bytes()).contains("zip64"));
        assert!(patch(6, &1u16.to_le_bytes()).contains("multi-disk"));
        let mut m = z.clone();
        m[eocd + 8..eocd + 12].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert!(read(&m, &Limits { entries: 100_000, ..Limits::DEFAULT }).unwrap_err().contains("zip64"));
        let mut m = z.clone();
        m[eocd + 8..eocd + 12].copy_from_slice(&[0, 0, 0, 0]);
        assert!(read(&m, &Limits::DEFAULT).unwrap_err().contains("size is wrong"));
    }

    // Checks the writer refuses more entries than a zip can hold.
    #[test]
    fn test_write_rejects_too_many_entries() {
        let entries = (0..0xffff).map(|i| Entry { name: format!("f{i}"), data: Vec::new() }).collect();
        assert!(write(&Archive::new(entries)).unwrap_err().contains("too many"));
    }
}
