// Holds back what llama.cpp and the GPU backend print to standard error while starting (device lists, tensor loading, context sizes), keeping it only when the step fails.
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;

// Lines kept from a failed step's captured output, at most.
const KEEP: usize = 20;

// Runs `f` with standard error sent to a temporary file; on failure the captured lines that look like errors are added to the message. ZAZZLE_LLM_LOG shows everything instead.
pub fn run<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if std::env::var_os("ZAZZLE_LLM_LOG").is_some() {
        return f();
    }
    let Some(mut file) = tempfile() else { return f() };
    // SAFETY: duplicating and redirecting descriptor 2 to a file we own; it is put back below.
    let saved = unsafe { libc::dup(2) };
    if saved < 0 || unsafe { libc::dup2(file.as_raw_fd(), 2) } < 0 {
        return f();
    }
    let result = f();
    // SAFETY: restores the descriptor saved above and closes the copy.
    unsafe {
        libc::dup2(saved, 2);
        libc::close(saved);
    }
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            let mut text = String::new();
            let _ = file.seek(SeekFrom::Start(0)).and_then(|_| file.read_to_string(&mut text));
            Err(with_errors(e, &text))
        }
    }
}

// Adds the captured lines that mention an error or failure to a message.
fn with_errors(msg: String, captured: &str) -> String {
    let lines: Vec<&str> = captured
        .lines()
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("fail") || l.contains("unable")
        })
        .collect();
    let tail = &lines[lines.len().saturating_sub(KEEP)..];
    if tail.is_empty() {
        msg
    } else {
        format!("{msg}\n{}", tail.join("\n"))
    }
}

// Makes an unnamed temporary file, removed when closed.
fn tempfile() -> Option<std::fs::File> {
    let path = std::env::temp_dir().join(format!(
        "ai-agent-log-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let file =
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Writes a line straight to descriptor 2, as llama.cpp's C code does.
    fn raw(line: &str) {
        let b = format!("{line}\n");
        // SAFETY: writes a buffer we own to descriptor 2.
        unsafe { libc::write(2, b.as_ptr().cast(), b.len()) };
    }

    // Checks a successful step drops what it printed, a failed one keeps only its error lines, and the stream is restored.
    #[test]
    fn test_quiet_run() {
        let ok = run(|| {
            raw("ggml_metal_init: found device");
            Ok(1)
        });
        assert_eq!(ok, Ok(1));
        let err = run::<()>(|| {
            raw("load_tensors: offloading 28 layers");
            raw("llama_model_load: error loading model: bad magic");
            Err("loading m.gguf: failed".into())
        });
        let e = err.expect_err("fails");
        assert!(
            e.starts_with("loading m.gguf: failed\n") && e.contains("bad magic") && !e.contains("offloading"),
            "{e}"
        );
        assert_eq!(with_errors("m".into(), "nothing here"), "m");
    }
}
