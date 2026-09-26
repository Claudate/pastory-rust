//! Port of `App/Sandbox.swift`.
//!
//! Test sandbox switches. They exist only for `--selftest` runs: a normal
//! launch ignores the environment entirely, so a stray `PASTORY_STORE` /
//! `PASTORY_LANG` in the launching shell can never redirect the app away from
//! the user's real history and settings.

use std::env;
use std::sync::OnceLock;

/// A bare `--selftest` is a normal launch; `--selftest <cmd>` is a test run.
pub fn is_self_test(args: &[String]) -> bool {
    match args.iter().position(|a| a == "--selftest") {
        Some(i) => i + 1 < args.len(),
        None => false,
    }
}

/// `PASTORY_STORE`, honoured only under `--selftest <cmd>`.
pub fn store(args: &[String], env: &[(String, String)]) -> Option<String> {
    if !is_self_test(args) {
        return None;
    }
    let v = env_get(env, "PASTORY_STORE")?;
    if v.is_empty() { None } else { Some(v.to_string()) }
}

/// `PASTORY_LANG`, honoured only under `--selftest <cmd>`.
pub fn language(args: &[String], env: &[(String, String)]) -> Option<String> {
    if !is_self_test(args) {
        return None;
    }
    env_get(env, "PASTORY_LANG").map(|s| s.to_string())
}

fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// The process environment snapshot + argv, resolved once at startup.
/// Passing these around explicitly keeps the "normal launch ignores the
/// environment" rule auditable: only `sandbox::` reads them.
pub struct Launch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Launch {
    /// Reads `std::env::args` and `std::env::vars` at startup; values are
    /// frozen so later `set_var` cannot change behaviour mid-run.
    pub fn capture() -> Launch {
        Launch {
            args: env::args().collect(),
            env: env::vars().collect(),
        }
    }

    pub fn is_self_test(&self) -> bool {
        is_self_test(&self.args)
    }

    pub fn store(&self) -> Option<String> {
        store(&self.args, &self.env)
    }

    pub fn language(&self) -> Option<String> {
        language(&self.args, &self.env)
    }
}

/// Process-wide frozen launch state, set by `main` before anything reads it.
static LAUNCH: OnceLock<Launch> = OnceLock::new();

pub fn init() {
    let _ = LAUNCH.set(Launch::capture());
}

pub fn launch() -> &'static Launch {
    LAUNCH.get().expect("sandbox::init() must run before use")
}
