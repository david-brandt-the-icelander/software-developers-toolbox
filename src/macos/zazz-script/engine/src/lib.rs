// zazz: the SDK for zazz scripts. Register handlers, call zazz::serve, and test them with zazz::test.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod call;
pub mod cliargs;
pub mod error;
#[cfg(unix)]
pub mod fdpass;
pub mod handlers;
pub mod http;
pub mod json;
#[cfg(unix)]
pub mod native;
pub mod proto;
pub mod render;
pub mod test;
#[cfg(target_family = "wasm")]
mod wasm;

pub use call::{Call, Mode};
pub use error::{Error, Result};
pub use handlers::Handlers;
pub use http::{Request, Response};
pub use json::Json;

pub const VERSION: &str = "0.1.0";

// Serves calls from the launcher until it recycles this process (native) or this instance's single call (WASM).
pub fn serve(h: Handlers) {
    #[cfg(target_family = "wasm")]
    wasm::serve(h);
    #[cfg(all(unix, not(target_family = "wasm")))]
    native::serve(h);
}
