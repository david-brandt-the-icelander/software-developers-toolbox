// The wasmtime engine: one configuration for compiling and running, import checks, and the epoch ticker.
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use wasmtime::{Config, Engine, Module};

pub const TICK_MS: u64 = 10;
pub const HOST_MODULE: &str = "zazz_host";
const WASI_MODULE: &str = "wasi_snapshot_preview1";
const HOST_FUNCS: [&str; 5] = ["request_len", "take", "send", "http", "cancelled"];

// The shared engine, configured identically for compile and run so precompiled modules load.
pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut c = Config::new();
        c.epoch_interruption(true);
        c.cranelift_opt_level(wasmtime::OptLevel::Speed);
        Engine::new(&c).expect("the fixed engine configuration is valid")
    })
}

// Lists imports outside the allowed set: WASI preview 1 and the zazz_host functions.
pub fn bad_imports(m: &Module) -> Vec<String> {
    m.imports()
        .filter(|i| {
            !(i.module() == WASI_MODULE || i.module() == HOST_MODULE && HOST_FUNCS.contains(&i.name()))
        })
        .map(|i| format!("{}.{}", i.module(), i.name()))
        .collect()
}

// Compiles WASM bytes, checks the imports and returns the precompiled module bytes.
pub fn compile(wasm: &[u8]) -> Result<Vec<u8>, String> {
    let m = Module::new(engine(), wasm).map_err(|e| format!("invalid WebAssembly: {e:#}"))?;
    let bad = bad_imports(&m);
    if !bad.is_empty() {
        return Err(format!("the module imports functions zazz does not provide: {}", bad.join(", ")));
    }
    if m.get_export("_start").is_none() {
        return Err("the module is not a WASI command (no _start export)".into());
    }
    m.serialize().map_err(|e| format!("serialising the compiled module: {e:#}"))
}

// Loads a module precompiled by `compile` with this same engine.
pub fn load(cwasm: &[u8]) -> Result<Module, String> {
    // SAFETY: the bytes come from `compile` in this binary, stored by zazzled in its private 0700 store.
    unsafe { Module::deserialize(engine(), cwasm) }.map_err(|e| format!("loading the compiled module: {e:#}"))
}

// Starts the background thread that advances the epoch, so call deadlines fire.
pub fn start_ticker() {
    static STARTED: OnceLock<Arc<()>> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(TICK_MS));
            engine().increment_epoch();
        });
        Arc::new(())
    });
}

// Converts a time limit in milliseconds to epoch ticks.
pub fn ticks(ms: u64) -> u64 {
    ms.div_ceil(TICK_MS).max(1)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    const OK: &str = r#"(module (import "zazz_host" "send" (func (param i32 i32))) (memory (export "memory") 1) (func (export "_start")))"#;

    // Checks a valid command compiles and loads back.
    #[test]
    fn test_compile_and_load() {
        let wasm = wat(OK);
        let cwasm = compile(&wasm).expect("compile");
        let m = load(&cwasm).expect("load");
        assert!(m.get_export("_start").is_some());
        assert!(load(b"junk").is_err());
    }

    // Checks bad bytes, unknown imports and missing _start are refused.
    #[test]
    fn test_compile_refusals() {
        assert!(compile(b"nope").unwrap_err().starts_with("invalid WebAssembly"));
        let other = wat(
            r#"(module (import "env" "f" (func)) (import "zazz_host" "nope" (func)) (func (export "_start")))"#,
        );
        assert_eq!(
            compile(&other).unwrap_err(),
            "the module imports functions zazz does not provide: env.f, zazz_host.nope"
        );
        let lib = wat("(module (func (export \"f\")))");
        assert!(compile(&lib).unwrap_err().contains("no _start"));
    }

    // Checks tick conversion and that the ticker starts once.
    #[test]
    fn test_ticks_and_ticker() {
        assert_eq!((ticks(0), ticks(10), ticks(11)), (1, 1, 2));
        start_ticker();
        start_ticker();
    }

    // Converts WebAssembly text to binary for tests.
    pub fn wat(text: &str) -> Vec<u8> {
        wat::parse_str(text).expect("valid test WAT")
    }
}
