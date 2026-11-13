// The launcher protocol: length-prefixed JSON frames and the messages exchanged over them.
use crate::error::Error;
use crate::json::Json;
use std::io::{self, Read, Write};

pub const PROTO: i64 = 1;
pub const MAX_FRAME: usize = 16 << 20;

// Writes one frame: u32 little-endian length, then compact JSON.
pub fn write_frame(w: &mut impl Write, v: &Json) -> io::Result<()> {
    let body = v.to_string();
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = (body.len() as u32).to_le_bytes().to_vec();
    buf.extend_from_slice(body.as_bytes());
    w.write_all(&buf)?;
    w.flush()
}

// Decodes a frame's length prefix, refusing oversized frames.
pub fn frame_len(header: [u8; 4]) -> io::Result<usize> {
    let n = u32::from_le_bytes(header) as usize;
    if n > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    Ok(n)
}

// Parses a frame body as JSON.
pub fn parse_body(body: &[u8]) -> io::Result<Json> {
    Json::parse_bytes(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.message))
}

// Reads one frame; returns None on a clean end of stream before a frame starts.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Json>> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = r.read(&mut header[got..])?;
        if n == 0 {
            return if got == 0 { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
        }
        got += n;
    }
    let mut body = vec![0u8; frame_len(header)?];
    r.read_exact(&mut body)?;
    parse_body(&body).map(Some)
}

// The launcher's first message.
pub fn hello() -> Json {
    crate::json!({"t": "hello", "proto": PROTO})
}

// The script's answer to hello: the messages it handles.
pub fn ready(messages: &[&str]) -> Json {
    let list: Vec<Json> = messages.iter().map(|m| Json::from(*m)).collect();
    crate::json!({"t": "ready", "messages": (list), "sdk": (crate::VERSION)})
}

// A successful result.
pub fn result_ok(id: i64, output: Json, exit: i64) -> Json {
    crate::json!({"t": "result", "id": id, "ok": true, "output": output, "exit": exit})
}

// A failed result.
pub fn result_err(id: i64, err: &Error) -> Json {
    crate::json!({"t": "result", "id": id, "ok": false, "error": (err.to_json()), "exit": 1})
}

// A progress note for a running call.
pub fn progress(id: i64, msg: &str) -> Json {
    crate::json!({"t": "progress", "id": id, "msg": msg})
}

// The message type of a frame, or "" when missing.
pub fn kind(v: &Json) -> &str {
    v.str_or("t", "")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks frames round-trip and a clean EOF reads as None.
    #[test]
    fn test_frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &hello()).expect("write");
        write_frame(&mut buf, &ready(&["json-command"])).expect("write");
        let mut r = &buf[..];
        assert_eq!(kind(&read_frame(&mut r).expect("read").expect("frame")), "hello");
        let ready = read_frame(&mut r).expect("read").expect("frame");
        assert_eq!(ready["messages"][0].str(), Some("json-command"));
        assert_eq!(read_frame(&mut r).expect("eof"), None);
    }

    // Checks truncated, oversized and malformed frames are errors.
    #[test]
    fn test_frame_errors() {
        assert!(read_frame(&mut &[1u8, 0][..]).is_err());
        assert!(read_frame(&mut &[5u8, 0, 0, 0, b'{'][..]).is_err());
        assert!(read_frame(&mut &[1u8, 0, 0, 0, b'{'][..]).is_err());
        assert!(frame_len(u32::MAX.to_le_bytes()).is_err());
        let huge = Json::Str("x".repeat(MAX_FRAME + 1));
        assert!(write_frame(&mut Vec::new(), &huge).is_err());
    }

    // Checks the message builders.
    #[test]
    fn test_messages() {
        assert_eq!(result_ok(3, Json::Int(1), 0)["ok"], Json::Bool(true));
        let e = result_err(3, &Error::failed("x"));
        assert_eq!((e["ok"].bool(), e["error"]["code"].str()), (Some(false), Some("failed")));
        assert_eq!(progress(1, "half")["msg"].str(), Some("half"));
        assert_eq!(kind(&Json::Null), "");
    }
}
