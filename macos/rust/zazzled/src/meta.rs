// Derived per-file data: hash, line index, content flags and per-line edit anchors.
use memchr::memchr;

pub const MAX_META_SIZE: u64 = 256 << 20;
const BINARY_SNIFF: usize = 8000;
const MINIFIED_LINE: usize = 5000;
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

// The light, persistable summary of a file's content; it carries no hash, so computing it never hashes the file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Info {
    pub size: u64,
    pub binary: bool,
    pub minified: bool,
    pub crlf: bool,
    pub lines: u32,
}

// Everything the commands need to know about a file's content without re-scanning it.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    pub size: u64,
    pub hash: [u8; 32],
    pub binary: bool,
    pub minified: bool,
    pub crlf: bool,
    pub starts: Vec<u32>,
}

// Returns the byte offset where each line starts; a trailing newline adds no empty line.
pub fn line_starts(data: &[u8]) -> Vec<u32> {
    let mut starts = Vec::new();
    if data.is_empty() {
        return starts;
    }
    starts.push(0);
    for pos in memchr::memchr_iter(b'\n', data) {
        if pos + 1 < data.len() {
            starts.push((pos + 1) as u32);
        }
    }
    starts
}

// Hashes a line's bytes into a 15-bit anchor.
fn anchor_of(line: &[u8]) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in line {
        h = (h ^ u32::from(*b)).wrapping_mul(0x0100_0193);
    }
    ((h ^ (h >> 15)) & 0x7fff) as u16
}

// Returns line n's (1-based) anchor, computed on demand because only read --anchors and edit --at need it.
pub fn line_anchor(data: &[u8], starts: &[u32], n: usize) -> u16 {
    anchor_of(line_bytes(data, starts, n))
}

// Renders an anchor as three base-32 characters.
pub fn anchor_str(a: u16) -> String {
    [(a >> 10) & 31, (a >> 5) & 31, a & 31].iter().map(|i| ALPHABET[usize::from(*i)] as char).collect()
}

// Returns true if the text looks binary.
pub fn looks_binary(data: &[u8]) -> bool {
    memchr(0, &data[..data.len().min(BINARY_SNIFF)]).is_some()
}

// Line count and the minified and CRLF flags of text, from one scan over its newlines.
struct TextFlags {
    lines: usize,
    minified: bool,
    crlf: bool,
}

// Scans text once: counts lines like line_starts, and measures the longest line and CRLF endings like line_bytes.
fn text_flags(data: &[u8]) -> TextFlags {
    let (mut lines, mut longest, mut crlf_lines, mut start) = (0, 0, 0, 0);
    for nl in memchr::memchr_iter(b'\n', data) {
        let cr = nl > start && data[nl - 1] == b'\r';
        lines += 1;
        crlf_lines += usize::from(cr);
        longest = longest.max(nl - start - usize::from(cr));
        start = nl + 1;
    }
    if start < data.len() {
        lines += 1;
        longest = longest.max(data.len() - start - usize::from(data.last() == Some(&b'\r')));
    }
    TextFlags { lines, minified: longest > MINIFIED_LINE, crlf: crlf_lines * 2 > lines }
}

impl Info {
    // Computes the summary that ls and find need: size, binary flag, line count and flags, without hashing or indexing lines.
    pub fn compute(data: &[u8]) -> Info {
        let size = data.len() as u64;
        if looks_binary(data) {
            return Info { size, binary: true, minified: false, crlf: false, lines: 0 };
        }
        let f = text_flags(data);
        Info { size, binary: false, minified: f.minified, crlf: f.crlf, lines: f.lines as u32 }
    }
}

impl Meta {
    // Computes the full metadata for file content: hash and line index as well as the summary flags.
    pub fn compute(data: &[u8]) -> Meta {
        let hash = *blake3::hash(data).as_bytes();
        let size = data.len() as u64;
        if looks_binary(data) {
            return Meta { size, hash, binary: true, minified: false, crlf: false, starts: vec![] };
        }
        let f = text_flags(data);
        Meta { size, hash, binary: false, minified: f.minified, crlf: f.crlf, starts: line_starts(data) }
    }

    // Returns the number of lines.
    pub fn lines(&self) -> usize {
        self.starts.len()
    }

    // Returns the light summary that is persisted in the cache.
    pub fn info(&self) -> Info {
        Info {
            size: self.size,
            binary: self.binary,
            minified: self.minified,
            crlf: self.crlf,
            lines: self.starts.len() as u32,
        }
    }
}

// Returns the offset just past a line's text, before its newline.
fn line_end(data: &[u8], starts: &[u32], n: usize) -> usize {
    if n < starts.len() {
        return starts[n] as usize - 1;
    }
    if data.last() == Some(&b'\n') {
        data.len() - 1
    } else {
        data.len()
    }
}

// Returns line n (1-based) without its newline or a trailing carriage return.
pub fn line_bytes<'a>(data: &'a [u8], starts: &[u32], n: usize) -> &'a [u8] {
    let start = starts[n - 1] as usize;
    let end = line_end(data, starts, n);
    let line = &data[start..end];
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks line starts for empty, plain and trailing-newline content.
    #[test]
    fn test_line_starts_cases() {
        assert!(line_starts(b"").is_empty());
        assert_eq!(line_starts(b"a\nb"), vec![0, 2]);
        assert_eq!(line_starts(b"a\nb\n"), vec![0, 2]);
        assert_eq!(line_starts(b"\n\n"), vec![0, 1]);
    }

    // Checks line extraction including the CRLF and last-line cases.
    #[test]
    fn test_line_bytes_cases() {
        let data = b"one\r\ntwo\nthree";
        let s = line_starts(data);
        assert_eq!(line_bytes(data, &s, 1), b"one");
        assert_eq!(line_bytes(data, &s, 2), b"two");
        assert_eq!(line_bytes(data, &s, 3), b"three");
        let data = b"x\n";
        assert_eq!(line_bytes(data, &line_starts(data), 1), b"x");
    }

    // Checks metadata for plain text.
    #[test]
    fn test_compute_text() {
        let m = Meta::compute(b"fn a() {}\nfn b() {}\n");
        assert_eq!((m.lines(), m.binary, m.minified, m.crlf), (2, false, false, false));
        let data = b"fn a() {}\nfn b() {}\n";
        assert_ne!(line_anchor(data, &m.starts, 1), line_anchor(data, &m.starts, 2));
        assert_eq!(m.size, 20);
    }

    // Checks binary detection skips the line index.
    #[test]
    fn test_compute_binary() {
        let m = Meta::compute(b"ab\0cd");
        assert!(m.binary && m.starts.is_empty());
        assert!(looks_binary(b"\0"));
        assert!(!looks_binary(b"text"));
    }

    // Checks CRLF majority detection.
    #[test]
    fn test_compute_crlf_majority() {
        assert!(Meta::compute(b"a\r\nb\r\nc\n").crlf);
        assert!(!Meta::compute(b"a\r\nb\nc\n").crlf);
        assert!(!Meta::compute(b"a\nb\n").crlf);
    }

    // Checks minified detection by line length.
    #[test]
    fn test_compute_minified() {
        let long = "x".repeat(MINIFIED_LINE + 1);
        assert!(Meta::compute(long.as_bytes()).minified);
        assert!(!Meta::compute(b"short").minified);
    }

    // Checks anchors differ for different text and render as three characters.
    #[test]
    fn test_anchor_rendering() {
        assert_ne!(anchor_of(b"a"), anchor_of(b"b"));
        assert_eq!(anchor_str(0), "000");
        assert_eq!(anchor_str(0x7fff), "zzz");
        assert_eq!(anchor_str(anchor_of(b"hello")).len(), 3);
        assert_eq!(line_anchor(b"x\r\nhello\n", &line_starts(b"x\r\nhello\n"), 2), anchor_of(b"hello"));
    }

    // Checks the persisted summary mirrors the metadata.
    #[test]
    fn test_info_mirrors_meta() {
        let m = Meta::compute(b"a\r\nb\r\n");
        let i = m.info();
        assert_eq!((i.lines, i.size, i.crlf, i.binary, i.minified), (2, 6, true, false, false));
    }

    // Checks the hash-free summary matches the full metadata's summary across line-ending and length edge cases.
    #[test]
    fn test_info_compute_matches_meta() {
        let long = format!("{}\nshort\n", "x".repeat(MINIFIED_LINE + 1));
        let edge = format!("{}\r\n", "y".repeat(MINIFIED_LINE));
        let cases: [&[u8]; 15] = [
            b"",
            b"a",
            b"a\n",
            b"a\nb",
            b"\n\n",
            b"\r\n",
            b"\r",
            b"a\r",
            b"a\r\nb\r\nc\n",
            b"a\r\nb\nc\n",
            b"\nabc\r\ndef\r\n",
            b"ab\0cd",
            long.as_bytes(),
            edge.as_bytes(),
            b"x\r\ny\r",
        ];
        for data in cases {
            assert_eq!(
                Info::compute(data),
                Meta::compute(data).info(),
                "{:?}",
                String::from_utf8_lossy(data)
            );
        }
    }

    // Checks the empty file has no lines.
    #[test]
    fn test_compute_empty() {
        let m = Meta::compute(b"");
        assert_eq!((m.lines(), m.size, m.binary), (0, 0, false));
    }

    // Checks CRLF detection with an empty first line.
    #[test]
    fn test_compute_empty_first_line() {
        let m = Meta::compute(b"\nabc\r\ndef\r\n");
        assert_eq!(m.lines(), 3);
        assert!(m.crlf);
    }
}
