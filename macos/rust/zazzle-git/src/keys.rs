// Turns the bytes a terminal sends into key presses, covering legacy sequences, the kitty keyboard protocol and bracketed paste.

// The keys the editor reacts to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Esc,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    F1,
    Paste(String),
    Ignored,
}

// One key press with its modifiers and whether the terminal reported it as an auto-repeat of a held key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub shift: bool,
    pub alt: bool,
    pub repeat: bool,
}

// What decoding the start of a byte buffer produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Key(KeyEvent, usize),
    Release(usize),
    Incomplete,
}

const PASTE_END: &[u8] = b"\x1b[201~";

// Wraps a key with no modifiers.
fn plain(key: Key) -> KeyEvent {
    KeyEvent { key, shift: false, alt: false, repeat: false }
}

// Decodes the first key in the buffer; a lone Esc is Incomplete until the caller's short wait for more bytes runs out.
pub fn decode(buf: &[u8]) -> Decoded {
    let Some(&b) = buf.first() else { return Decoded::Incomplete };
    match b {
        0x1b => decode_escape(buf),
        _ => decode_simple(buf),
    }
}

// Decodes a lone Esc after the wait for more bytes ran out.
pub fn lone_escape() -> KeyEvent {
    plain(Key::Esc)
}

// Decodes a key that does not start with Esc: control bytes and UTF-8 characters.
fn decode_simple(buf: &[u8]) -> Decoded {
    let b = buf[0];
    let key = match b {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0x00 => Key::Ignored,
        0x01..=0x1a => Key::Ctrl((b'a' + b - 1) as char),
        0x1c..=0x1f => Key::Ignored,
        _ => return decode_utf8(buf),
    };
    Decoded::Key(plain(key), 1)
}

// Decodes one UTF-8 character, skipping a byte that cannot start one.
fn decode_utf8(buf: &[u8]) -> Decoded {
    let len = match buf[0] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Decoded::Key(plain(Key::Ignored), 1),
    };
    if buf.len() < len {
        return Decoded::Incomplete;
    }
    match std::str::from_utf8(&buf[..len]).ok().and_then(|s| s.chars().next()) {
        Some(c) => Decoded::Key(plain(Key::Char(c)), len),
        None => Decoded::Key(plain(Key::Ignored), 1),
    }
}

// Decodes a sequence that starts with Esc: CSI, SS3 or an Alt-modified key.
fn decode_escape(buf: &[u8]) -> Decoded {
    let Some(&next) = buf.get(1) else { return Decoded::Incomplete };
    match next {
        b'[' => decode_csi(buf),
        b'O' => decode_ss3(buf),
        0x1b => Decoded::Key(plain(Key::Esc), 1),
        _ => decode_alt(buf),
    }
}

// Decodes Esc followed by a key as that key with Alt held.
fn decode_alt(buf: &[u8]) -> Decoded {
    match decode_simple(&buf[1..]) {
        Decoded::Key(mut ev, n) => {
            ev.alt = true;
            if let Key::Char(c) = ev.key {
                ev.key = Key::Alt(c.to_ascii_lowercase());
            }
            Decoded::Key(ev, n + 1)
        }
        other => other,
    }
}

// Decodes SS3 sequences: application-mode arrows and F1.
fn decode_ss3(buf: &[u8]) -> Decoded {
    let Some(&c) = buf.get(2) else { return Decoded::Incomplete };
    let key = match c {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'P' => Key::F1,
        _ => Key::Ignored,
    };
    Decoded::Key(plain(key), 3)
}

// Decodes a CSI sequence: its parameters, then the final byte that names the key.
fn decode_csi(buf: &[u8]) -> Decoded {
    let Some(end) = buf[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
        return if buf.len() > 32 {
            Decoded::Key(plain(Key::Ignored), buf.len())
        } else {
            Decoded::Incomplete
        };
    };
    let end = end + 2;
    let params = String::from_utf8_lossy(&buf[2..end]).into_owned();
    let n = end + 1;
    if buf[end] == b'~' && params == "200" {
        return decode_paste(buf, n);
    }
    let fields: Vec<&str> = params.split(';').collect();
    let first = fields[0].split(':').next().unwrap_or("").parse::<u32>().unwrap_or(1);
    let (mods, event) = modifiers(fields.get(1).copied().unwrap_or("1"));
    if event == 3 {
        return Decoded::Release(n);
    }
    let key = csi_key(buf[end], first, mods);
    let ev = KeyEvent { key, shift: mods & 1 != 0, alt: mods & 2 != 0, repeat: event == 2 };
    Decoded::Key(ev, n)
}

// Splits a CSI modifier field `mods[:event]` into the modifier bits (shift 1, alt 2, ctrl 4) and the kitty event type.
fn modifiers(field: &str) -> (u32, u32) {
    let mut parts = field.split(':');
    let mods = parts.next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(1).saturating_sub(1);
    let event = parts.next().and_then(|e| e.parse::<u32>().ok()).unwrap_or(1);
    (mods, event)
}

// Names the key of a CSI sequence from its final byte, first parameter and modifiers.
fn csi_key(fin: u8, first: u32, mods: u32) -> Key {
    match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'P' => Key::F1,
        b'Z' => Key::BackTab,
        b'~' => tilde_key(first),
        b'u' => kitty_key(first, mods),
        _ => Key::Ignored,
    }
}

// Names the key of a `CSI n ~` sequence.
fn tilde_key(n: u32) -> Key {
    match n {
        3 => Key::Delete,
        5 => Key::PageUp,
        6 => Key::PageDown,
        11 => Key::F1,
        _ => Key::Ignored,
    }
}

// Names the key of a kitty `CSI code;mods u` sequence.
fn kitty_key(code: u32, mods: u32) -> Key {
    let ctrl = mods & 4 != 0;
    let alt = mods & 2 != 0;
    match code {
        27 => Key::Esc,
        13 => Key::Enter,
        9 if mods & 1 != 0 => Key::BackTab,
        9 => Key::Tab,
        127 | 8 => Key::Backspace,
        _ => match char::from_u32(code) {
            Some(c) if ctrl && c.is_ascii_alphabetic() => Key::Ctrl(c.to_ascii_lowercase()),
            Some(c) if alt && c.is_ascii_alphabetic() => Key::Alt(c.to_ascii_lowercase()),
            Some(c) if !ctrl && !alt && !c.is_control() => Key::Char(c),
            _ => Key::Ignored,
        },
    }
}

// Collects bracketed-paste text up to the end marker, or reports Incomplete until it arrives.
fn decode_paste(buf: &[u8], start: usize) -> Decoded {
    let rest = &buf[start..];
    let Some(at) = rest.windows(PASTE_END.len()).position(|w| w == PASTE_END) else {
        return Decoded::Incomplete;
    };
    let text = String::from_utf8_lossy(&rest[..at]).replace("\r\n", "\n").replace('\r', "\n");
    Decoded::Key(plain(Key::Paste(text)), start + at + PASTE_END.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Returns the key and length decoded from the bytes, panicking on anything else.
    fn key(bytes: &[u8]) -> (KeyEvent, usize) {
        match decode(bytes) {
            Decoded::Key(ev, n) => (ev, n),
            other => panic!("{other:?}"),
        }
    }

    // Checks control bytes and plain characters.
    #[test]
    fn test_decode_simple_keys() {
        assert_eq!(key(b"\r").0.key, Key::Enter);
        assert_eq!(key(b"\n").0.key, Key::Enter);
        assert_eq!(key(b"\t").0.key, Key::Tab);
        assert_eq!(key(b"\x7f").0.key, Key::Backspace);
        assert_eq!(key(b"\x08").0.key, Key::Backspace);
        assert_eq!(key(b"\x00").0.key, Key::Ignored);
        assert_eq!(key(b"\x13").0.key, Key::Ctrl('s'));
        assert_eq!(key(b"\x1d").0.key, Key::Ignored);
        assert_eq!(key(b"a"), (plain(Key::Char('a')), 1));
        assert_eq!(key("é".as_bytes()), (plain(Key::Char('é')), 2));
        assert_eq!(key("€".as_bytes()).1, 3);
        assert_eq!(key("😀".as_bytes()).1, 4);
        assert_eq!(key(b"\xff").0.key, Key::Ignored);
        assert_eq!(key(b"\xc3\x28").0.key, Key::Ignored);
        assert_eq!(decode("é".as_bytes()[..1].as_ref()), Decoded::Incomplete);
        assert_eq!(decode(b""), Decoded::Incomplete);
    }

    // Checks Esc handling: lone, doubled and Alt-modified keys.
    #[test]
    fn test_decode_escape_and_alt() {
        assert_eq!(decode(b"\x1b"), Decoded::Incomplete);
        assert_eq!(lone_escape(), plain(Key::Esc));
        assert_eq!(key(b"\x1b\x1b").0.key, Key::Esc);
        let (ev, n) = key(b"\x1bA");
        assert_eq!((ev.key, ev.alt, n), (Key::Alt('a'), true, 2));
        let (ev, _) = key(b"\x1b\x7f");
        assert_eq!((ev.key, ev.alt), (Key::Backspace, true));
        assert_eq!(decode(b"\x1b\xc3"), Decoded::Incomplete);
    }

    // Checks arrows, function and editing keys in CSI and SS3 form, with modifiers.
    #[test]
    fn test_decode_csi_and_ss3() {
        assert_eq!(key(b"\x1b[A").0.key, Key::Up);
        assert_eq!(key(b"\x1b[B").0.key, Key::Down);
        assert_eq!(key(b"\x1b[C").0.key, Key::Right);
        let (ev, n) = key(b"\x1b[1;2D");
        assert_eq!((ev.key, ev.shift, ev.alt, n), (Key::Left, true, false, 6));
        assert!(key(b"\x1b[1;3A").0.alt);
        assert_eq!(key(b"\x1bOA").0.key, Key::Up);
        assert_eq!(key(b"\x1bOB").0.key, Key::Down);
        assert_eq!(key(b"\x1bOC").0.key, Key::Right);
        assert_eq!(key(b"\x1bOD").0.key, Key::Left);
        assert_eq!(key(b"\x1bOP").0.key, Key::F1);
        assert_eq!(key(b"\x1bOx").0.key, Key::Ignored);
        assert_eq!(decode(b"\x1bO"), Decoded::Incomplete);
        assert_eq!(key(b"\x1b[11~").0.key, Key::F1);
        assert_eq!(key(b"\x1b[P").0.key, Key::F1);
        assert_eq!(key(b"\x1b[3~").0.key, Key::Delete);
        assert_eq!(key(b"\x1b[5~").0.key, Key::PageUp);
        assert_eq!(key(b"\x1b[6~").0.key, Key::PageDown);
        assert_eq!(key(b"\x1b[9~").0.key, Key::Ignored);
        assert_eq!(key(b"\x1b[Z").0.key, Key::BackTab);
        assert_eq!(key(b"\x1b[H").0.key, Key::Ignored);
        assert_eq!(decode(b"\x1b[1;2"), Decoded::Incomplete);
        let long = [b"\x1b[".as_slice(), &[b'1'; 40]].concat();
        assert_eq!(key(&long).0.key, Key::Ignored);
    }

    // Checks kitty keyboard protocol keys, repeats and releases.
    #[test]
    fn test_decode_kitty() {
        assert_eq!(key(b"\x1b[27u").0.key, Key::Esc);
        assert_eq!(key(b"\x1b[13u").0.key, Key::Enter);
        assert_eq!(key(b"\x1b[9u").0.key, Key::Tab);
        assert_eq!(key(b"\x1b[9;2u").0.key, Key::BackTab);
        assert_eq!(key(b"\x1b[127u").0.key, Key::Backspace);
        assert_eq!(key(b"\x1b[115;5u").0.key, Key::Ctrl('s'));
        assert_eq!(key(b"\x1b[97;3u").0.key, Key::Alt('a'));
        assert_eq!(key(b"\x1b[97u").0.key, Key::Char('a'));
        assert_eq!(key(b"\x1b[49;3u").0.key, Key::Ignored);
        assert_eq!(key(b"\x1b[57441;5u").0.key, Key::Ignored);
        assert_eq!(key(b"\x1b[1u").0.key, Key::Ignored);
        let (ev, _) = key(b"\x1b[1;1:2C");
        assert_eq!((ev.key, ev.repeat), (Key::Right, true));
        assert_eq!(decode(b"\x1b[1;1:3C"), Decoded::Release(8));
        assert_eq!(key(b"\x1b[;C").0.key, Key::Right);
    }

    // Checks bracketed paste, including a paste that has not fully arrived.
    #[test]
    fn test_decode_paste() {
        let (ev, n) = key(b"\x1b[200~a\r\nb\rc\x1b[201~x");
        assert_eq!(ev.key, Key::Paste("a\nb\nc".into()));
        assert_eq!(n, 18);
        assert_eq!(decode(b"\x1b[200~abc"), Decoded::Incomplete);
    }
}
