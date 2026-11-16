// The terminal: raw mode on /dev/tty with restore on exit, panic or signal, its size, and the loop that feeds keys to the editor.

use crate::editor::{Editor, Outcome};
use crate::keys::{decode, lone_escape, Decoded};
use crate::render::{frame, Scroll};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

// How long to wait after a lone Esc for the rest of a key sequence.
const ESC_WAIT_MS: i32 = 25;
// Alternate screen, bracketed paste, and the kitty keyboard protocol (disambiguate keys, report repeats).
const ENTER: &str = "\x1b[?1049h\x1b[?2004h\x1b[>3u\x1b[H\x1b[2J";
// Undoes ENTER: pops the keyboard mode, ends bracketed paste, leaves the alternate screen and shows the cursor.
const LEAVE: &str = "\x1b[<u\x1b[?2004l\x1b[?1049l\x1b[?25h";

static RESIZED: AtomicBool = AtomicBool::new(false);
static STOPPED: AtomicBool = AtomicBool::new(false);
static mut SAVED: Option<(i32, libc::termios)> = None;

// Notes a window size change.
extern "C" fn on_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

// Notes a request to stop (hangup, terminate, interrupt).
extern "C" fn on_stop(_: libc::c_int) {
    STOPPED.store(true, Ordering::SeqCst);
}

// Installs a signal handler without SA_RESTART, so a blocking poll returns when the signal arrives.
fn handle_signal(sig: libc::c_int, f: extern "C" fn(libc::c_int)) {
    // SAFETY: sigaction is called with a zeroed, then filled struct and a handler that only touches atomics.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = f as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

// Restores the saved terminal settings and screen modes; safe to call more than once.
fn restore() {
    // SAFETY: SAVED is written once before raw mode starts and only read here, on the main thread or in the panic hook.
    unsafe {
        if let Some((fd, t)) = (*std::ptr::addr_of!(SAVED)).as_ref() {
            libc::write(*fd, LEAVE.as_ptr().cast(), LEAVE.len());
            libc::tcsetattr(*fd, libc::TCSAFLUSH, t);
        }
    }
}

// The open terminal in raw mode; dropping it restores the terminal.
pub struct Term {
    tty: File,
}

impl Term {
    // Opens /dev/tty, saves its settings and switches it to raw mode with the editor's screen modes.
    pub fn open() -> io::Result<Term> {
        let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let fd = tty.as_raw_fd();
        // SAFETY: tcgetattr and tcsetattr get a valid fd and termios structs that live for the call.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            *std::ptr::addr_of_mut!(SAVED) = Some((fd, t));
            let mut raw = t;
            raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
            raw.c_oflag &= !libc::OPOST;
            raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG | libc::IEXTEN);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(fd, libc::TCSAFLUSH, &raw);
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            prev(info);
        }));
        handle_signal(libc::SIGWINCH, on_winch);
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            handle_signal(sig, on_stop);
        }
        let mut term = Term { tty };
        term.tty.write_all(ENTER.as_bytes())?;
        Ok(term)
    }

    // Returns the terminal size in columns and rows.
    fn size(&self) -> (usize, usize) {
        // SAFETY: TIOCGWINSZ fills the zeroed winsize struct for a valid fd.
        let ws = unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            libc::ioctl(self.tty.as_raw_fd(), libc::TIOCGWINSZ, &mut ws);
            ws
        };
        if ws.ws_col == 0 || ws.ws_row == 0 {
            return (80, 24);
        }
        (ws.ws_col as usize, ws.ws_row as usize)
    }

    // Waits up to the timeout (-1 forever) for input; returns true if there is some.
    fn ready(&self, timeout_ms: i32) -> bool {
        let mut p = libc::pollfd { fd: self.tty.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: poll gets one valid pollfd.
        unsafe { libc::poll(&mut p, 1, timeout_ms) > 0 }
    }

    // Runs the editor until it saves or cancels, or a signal stops it (which cancels).
    pub fn run(&mut self, ed: &mut Editor) -> io::Result<Outcome> {
        let start = Instant::now();
        let mut scroll = Scroll::default();
        let mut pending: Vec<u8> = Vec::new();
        loop {
            if STOPPED.load(Ordering::SeqCst) {
                return Ok(Outcome::Cancel);
            }
            RESIZED.store(false, Ordering::SeqCst);
            let (w, h) = self.size();
            let mut out = frame(ed, &mut scroll, w, h);
            if let Some(text) = ed.osc52.take() {
                out.push_str(&osc52(&text));
            }
            self.tty.write_all(out.as_bytes())?;
            if !self.ready(-1) {
                continue;
            }
            let mut chunk = [0u8; 4096];
            let n = self.tty.read(&mut chunk)?;
            if n == 0 {
                return Ok(Outcome::Cancel);
            }
            pending.extend_from_slice(&chunk[..n]);
            let now = start.elapsed().as_millis() as u64;
            if let Some(o) = feed(ed, &mut pending, now) {
                return Ok(o);
            }
            if pending == [0x1b] && !self.ready(ESC_WAIT_MS) {
                pending.clear();
                if let Some(o) = ed.handle(lone_escape(), now) {
                    return Ok(o);
                }
            }
        }
    }
}

impl Drop for Term {
    // Restores the terminal.
    fn drop(&mut self) {
        restore();
    }
}

// Decodes and handles every complete key in the pending bytes, keeping an incomplete tail; returns an outcome if editing ended.
pub fn feed(ed: &mut Editor, pending: &mut Vec<u8>, now_ms: u64) -> Option<Outcome> {
    loop {
        match decode(pending) {
            Decoded::Key(ev, n) => {
                pending.drain(..n);
                if let Some(o) = ed.handle(ev, now_ms) {
                    return Some(o);
                }
            }
            Decoded::Release(n) => {
                pending.drain(..n);
            }
            Decoded::Incomplete => return None,
        }
    }
}

// Returns the OSC 52 sequence that asks the terminal to put text on the system clipboard (works over SSH where supported).
pub fn osc52(text: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::new();
    for chunk in text.as_bytes().chunks(3) {
        let v = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                b64.push(TABLE[(v >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                b64.push('=');
            }
        }
    }
    format!("\x1b]52;c;{b64}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks the base64 encoding inside the OSC 52 sequence.
    #[test]
    fn test_osc52() {
        assert_eq!(osc52(""), "\x1b]52;c;\x07");
        assert_eq!(osc52("f"), "\x1b]52;c;Zg==\x07");
        assert_eq!(osc52("fo"), "\x1b]52;c;Zm8=\x07");
        assert_eq!(osc52("foobar"), "\x1b]52;c;Zm9vYmFy\x07");
    }
}
