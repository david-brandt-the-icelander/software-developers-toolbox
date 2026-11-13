// Wire format shared by the daemon, the C invoker and the MCP front end.
use std::io::{self, Read, Write};

pub const MAX_REQUEST: usize = 1 << 20;

// A decoded request: working directory, agent session and the command line.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub cwd: String,
    pub session: String,
    pub args: Vec<String>,
}

// Decodes the NUL-terminated field block into a request.
pub fn decode(block: &[u8]) -> Result<Request, String> {
    let mut parts: Vec<&[u8]> = block.split(|b| *b == 0).collect();
    parts.pop();
    if parts.len() < 2 {
        return Err("malformed request".into());
    }
    let mut fields = Vec::with_capacity(parts.len());
    for p in parts {
        fields.push(String::from_utf8(p.to_vec()).map_err(|_| "argument is not valid UTF-8".to_string())?);
    }
    let mut it = fields.into_iter();
    let cwd = it.next().unwrap_or_default();
    let session = it.next().unwrap_or_default();
    Ok(Request { cwd, session, args: it.collect() })
}

// Encodes a request as a length-prefixed block of NUL-terminated fields.
pub fn encode(req: &Request) -> Vec<u8> {
    let mut body = Vec::new();
    for f in [&req.cwd, &req.session].into_iter().chain(req.args.iter()) {
        body.extend_from_slice(f.as_bytes());
        body.push(0);
    }
    let mut out = (body.len() as u32).to_ne_bytes().to_vec();
    out.extend(body);
    out
}

// Reads the length prefix and returns it, rejecting oversize requests.
pub fn frame_len(header: [u8; 4]) -> Result<usize, String> {
    let n = u32::from_ne_bytes(header) as usize;
    if n > MAX_REQUEST {
        return Err("request too large".into());
    }
    Ok(n)
}

// Sends a request and reads the relay response: a status byte then the text.
pub fn roundtrip<S: Read + Write>(stream: &mut S, req: &Request) -> io::Result<(u8, String)> {
    stream.write_all(&encode(req))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let Some((status, body)) = buf.split_first() else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "daemon closed without answering"));
    };
    Ok((*status, String::from_utf8_lossy(body).into_owned()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    // Checks that encode followed by decode round-trips every field.
    #[test]
    fn test_encode_decode_roundtrip() {
        let req = Request {
            cwd: "/w".into(),
            session: "s1".into(),
            args: vec!["find".into(), "".into(), "x".into()],
        };
        let bytes = encode(&req);
        let n = frame_len([bytes[0], bytes[1], bytes[2], bytes[3]]).expect("len");
        assert_eq!(n, bytes.len() - 4);
        assert_eq!(decode(&bytes[4..]).expect("decode"), req);
    }

    // Checks that a request with no command arguments still decodes.
    #[test]
    fn test_decode_without_args() {
        let req = decode(b"\0\0").expect("decode");
        assert_eq!(req, Request { cwd: String::new(), session: String::new(), args: vec![] });
    }

    // Checks that too few fields are rejected.
    #[test]
    fn test_decode_rejects_short_block() {
        assert!(decode(b"only\0").is_err());
        assert!(decode(b"").is_err());
    }

    // Checks that invalid UTF-8 is rejected with a clear message.
    #[test]
    fn test_decode_rejects_invalid_utf8() {
        assert_eq!(decode(b"\0\0\xff\0").unwrap_err(), "argument is not valid UTF-8");
    }

    // Checks the size limit on frames.
    #[test]
    fn test_frame_len_limit() {
        assert_eq!(frame_len(10u32.to_ne_bytes()), Ok(10));
        assert!(frame_len(((MAX_REQUEST + 1) as u32).to_ne_bytes()).is_err());
    }

    // Checks the relay roundtrip against a fake server.
    #[test]
    fn test_roundtrip_reads_status_and_body() {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        let t = std::thread::spawn(move || {
            let mut hdr = [0u8; 4];
            b.read_exact(&mut hdr).expect("hdr");
            let mut body = vec![0u8; u32::from_ne_bytes(hdr) as usize];
            b.read_exact(&mut body).expect("body");
            b.write_all(&[3]).expect("status");
            b.write_all(b"hello").expect("text");
        });
        let req = Request { cwd: "/".into(), session: String::new(), args: vec!["x".into()] };
        let (status, text) = roundtrip(&mut a, &req).expect("roundtrip");
        t.join().expect("join");
        assert_eq!((status, text.as_str()), (3, "hello"));
    }

    // Checks that an empty response is an error.
    #[test]
    fn test_roundtrip_empty_response_errors() {
        let (mut a, b) = UnixStream::pair().expect("pair");
        drop(b);
        let req = Request { cwd: "/".into(), session: String::new(), args: vec![] };
        let err = roundtrip(&mut a, &req).unwrap_err();
        assert!(matches!(err.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe));
    }

    // Checks a server that reads the request and hangs up gives an unexpected-eof error.
    #[test]
    fn test_roundtrip_server_hangs_up() {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        let t = std::thread::spawn(move || {
            let mut hdr = [0u8; 4];
            b.read_exact(&mut hdr).expect("hdr");
            let mut body = vec![0u8; u32::from_ne_bytes(hdr) as usize];
            b.read_exact(&mut body).expect("body");
        });
        let req = Request { cwd: "/".into(), session: String::new(), args: vec![] };
        let err = roundtrip(&mut a, &req).unwrap_err();
        t.join().expect("join");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
