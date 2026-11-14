// The error type scripts return: a stable code the launcher understands plus a human message.
use crate::json::Json;

// An error with a machine-readable code and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
}

// The result type used throughout the SDK.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    // An error with any code.
    pub fn new(code: &str, message: &str) -> Error {
        Error { code: code.to_string(), message: message.to_string() }
    }

    // The input was malformed or missing a field.
    pub fn bad_input(message: &str) -> Error {
        Error::new("bad_input", message)
    }

    // Something the input named does not exist.
    pub fn not_found(message: &str) -> Error {
        Error::new("not_found", message)
    }

    // The script could not do what was asked.
    pub fn failed(message: &str) -> Error {
        Error::new("failed", message)
    }

    // The message or operation is not supported here.
    pub fn unsupported(message: &str) -> Error {
        Error::new("unsupported", message)
    }

    // The sandbox refused a filesystem access.
    pub fn sandbox_denied(message: &str) -> Error {
        Error::new("sandbox_denied", message)
    }

    // The sandbox refused a network request.
    pub fn network_denied(message: &str) -> Error {
        Error::new("network_denied", message)
    }

    // The call was cancelled by the caller.
    pub fn cancelled() -> Error {
        Error::new("cancelled", "the call was cancelled")
    }

    // Renders the error as {"code","message"}.
    pub fn to_json(&self) -> Json {
        crate::json!({"code": (&self.code), "message": (&self.message)})
    }

    // Reads an error from {"code","message"}, defaulting the code to "failed".
    pub fn from_json(v: &Json) -> Error {
        Error::new(v.str_or("code", "failed"), v.str_or("message", "unknown error"))
    }
}

impl std::fmt::Display for Error {
    // Prints "code: message".
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    // Maps I/O errors: permission problems become sandbox_denied, missing files not_found.
    fn from(e: std::io::Error) -> Error {
        match e.kind() {
            std::io::ErrorKind::NotFound => Error::not_found(&e.to_string()),
            std::io::ErrorKind::PermissionDenied => Error::sandbox_denied(&e.to_string()),
            _ => Error::failed(&e.to_string()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    // Checks constructors, JSON round-trip and display.
    #[test]
    fn test_error_constructors_and_json() {
        let all = [
            Error::bad_input("m"),
            Error::not_found("m"),
            Error::failed("m"),
            Error::unsupported("m"),
            Error::sandbox_denied("m"),
            Error::network_denied("m"),
            Error::cancelled(),
        ];
        for e in &all {
            assert_eq!(&Error::from_json(&e.to_json()), e);
        }
        assert_eq!(all[0].to_string(), "bad_input: m");
        assert_eq!(Error::from_json(&Json::Null), Error::failed("unknown error"));
    }

    // Checks I/O error kinds map to codes.
    #[test]
    fn test_error_from_io() {
        let code = |k: ErrorKind| Error::from(std::io::Error::new(k, "x")).code;
        assert_eq!(code(ErrorKind::NotFound), "not_found");
        assert_eq!(code(ErrorKind::PermissionDenied), "sandbox_denied");
        assert_eq!(code(ErrorKind::Other), "failed");
    }
}
