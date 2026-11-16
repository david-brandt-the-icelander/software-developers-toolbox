// Copy and paste through the system clipboard command (pbcopy, wl-copy, xclip), falling back to the editor's own clipboard.

use crate::git::run_with_input;
use std::time::Duration;

// How long a clipboard command may take.
const CLIP_TIMEOUT: Duration = Duration::from_secs(2);

// Something the editor can copy text to and paste text from.
pub trait Clipboard {
    // Stores text.
    fn copy(&mut self, text: &str);
    // Returns the stored text, if any.
    fn paste(&mut self) -> Option<String>;
}

// The system clipboard commands, if found, plus an in-memory copy used when they fail.
#[derive(Debug, Default)]
pub struct SystemClipboard {
    copy_cmd: Option<Vec<String>>,
    paste_cmd: Option<Vec<String>>,
    memory: Option<String>,
}

// Turns a command line of words into owned strings.
fn words(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl SystemClipboard {
    // Creates a clipboard that runs the given copy and paste commands.
    pub fn with_commands(copy_cmd: Option<Vec<String>>, paste_cmd: Option<Vec<String>>) -> SystemClipboard {
        SystemClipboard { copy_cmd, paste_cmd, memory: None }
    }

    // Picks the clipboard commands for this system from the OS and the display environment variables.
    pub fn detect(macos: bool, wayland: bool, x11: bool) -> SystemClipboard {
        let (c, p): (&[&str], &[&str]) = if macos {
            (&["pbcopy"], &["pbpaste"])
        } else if wayland {
            (&["wl-copy"], &["wl-paste", "-n"])
        } else if x11 {
            (&["xclip", "-selection", "clipboard"], &["xclip", "-selection", "clipboard", "-o"])
        } else {
            return SystemClipboard::default();
        };
        SystemClipboard::with_commands(Some(words(c)), Some(words(p)))
    }

    // Picks the clipboard commands for the running system.
    pub fn for_this_system() -> SystemClipboard {
        let env = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        SystemClipboard::detect(cfg!(target_os = "macos"), env("WAYLAND_DISPLAY"), env("DISPLAY"))
    }
}

impl Clipboard for SystemClipboard {
    // Stores the text in memory and hands it to the copy command.
    fn copy(&mut self, text: &str) {
        self.memory = Some(text.to_string());
        if let Some(cmd) = &self.copy_cmd {
            let _ = run_with_input(cmd, text, CLIP_TIMEOUT);
        }
    }

    // Returns the paste command's output, or the in-memory copy when there is no command or it fails.
    fn paste(&mut self) -> Option<String> {
        let from_system = self.paste_cmd.as_ref().and_then(|cmd| run_with_input(cmd, "", CLIP_TIMEOUT).ok());
        from_system.filter(|s| !s.is_empty()).or_else(|| self.memory.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Checks which commands are picked per system.
    #[test]
    fn test_clipboard_detect() {
        assert_eq!(SystemClipboard::detect(true, false, false).copy_cmd, Some(words(&["pbcopy"])));
        assert_eq!(SystemClipboard::detect(false, true, true).paste_cmd, Some(words(&["wl-paste", "-n"])));
        assert_eq!(SystemClipboard::detect(false, false, true).copy_cmd.map(|c| c.len()), Some(3));
        assert_eq!(SystemClipboard::detect(false, false, false).copy_cmd, None);
        let _ = SystemClipboard::for_this_system();
    }

    // Checks copying and pasting through commands, and the in-memory fallback.
    #[test]
    fn test_clipboard_commands_and_fallback() {
        let dir = std::env::temp_dir().join(format!("zg-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("clip");
        let f = file.to_str().unwrap().to_string();
        let mut c = SystemClipboard::with_commands(
            Some(vec!["sh".into(), "-c".into(), format!("cat > {f}")]),
            Some(vec!["cat".into(), f.clone()]),
        );
        c.copy("hello");
        assert_eq!(c.paste().as_deref(), Some("hello"));
        let mut m = SystemClipboard::with_commands(None, Some(vec!["false".into()]));
        assert_eq!(m.paste(), None);
        m.copy("mem");
        assert_eq!(m.paste().as_deref(), Some("mem"));
        let mut missing = SystemClipboard::with_commands(Some(vec!["/nonexistent/x".into()]), None);
        missing.copy("y");
        assert_eq!(missing.paste().as_deref(), Some("y"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
