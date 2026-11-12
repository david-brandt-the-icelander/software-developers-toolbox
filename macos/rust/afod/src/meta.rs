// Derived per-file data: hash, line index, edit anchors and content flags.
use memchr::memchr;

pub const MAX_META_SIZE: u64 = 256 << 20;
const BINARY_SNIFF: usize = 8000;
const MINIFIED_LINE: usize = 5000;
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

// Everything the commands need to know about a file's content without re-scanning it.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    pub size: u64,
    pub hash: [u8; 32],
    pub binary: bool,
    pub minified: bool,
    pub crlf: bool,
    pub starts: Vec<u32>,
    pub anchors: Vec<u16>,
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

// Renders an anchor as three base-32 characters.
pub fn anchor_str(a: u16) -> String {
    [(a >> 10) & 31, (a >> 5) & 31, a & 31].iter().map(|i| ALPHABET[usize::from(*i)] as char).collect()
}

// Returns true if the text looks binary.
pub fn looks_binary(data: &[u8]) -> bool {
    memchr(0, &data[..data.len().min(BINARY_SNIFF)]).is_some()
}

impl Meta {
    // Computes the metadata for file content.
    pub fn compute(data: &[u8]) -> Meta {
        let hash = *blake3::hash(data).as_bytes();
        let size = data.len() as u64;
        if looks_binary(data) {
            return Meta {
                size,
                hash,
                binary: true,
                minified: false,
                crlf: false,
                starts: vec![],
                anchors: vec![],
            };
        }
        let starts = line_starts(data);
        let mut anchors = Vec::with_capacity(starts.len());
        let mut longest = 0;
        let mut crlf_lines = 0;
        for n in 1..=starts.len() {
            let line = line_bytes(data, &starts, n);
            longest = longest.max(line.len());
            anchors.push(anchor_of(line));
            let end = line_end(data, &starts, n);
            if end < data.len() && end > 0 && data[end - 1] == b'\r' {
                crlf_lines += 1;
            }
        }
        let crlf = crlf_lines * 2 > starts.len();
        Meta { size, hash, binary: false, minified: longest > MINIFIED_LINE, crlf, starts, anchors }
    }

    // Returns the number of lines.
    pub fn lines(&self) -> usize {
        self.starts.len()
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
        assert_eq!(m.anchors.len(), 2);
        assert_ne!(m.anchors[0], m.anchors[1]);
        assert_eq!(m.size, 20);
    }

    // Checks binary detection skips the line index.
    #[test]
    fn test_compute_binary() {
        let m = Meta::compute(b"ab\0cd");
        assert!(m.binary && m.starts.is_empty() && m.anchors.is_empty());
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
