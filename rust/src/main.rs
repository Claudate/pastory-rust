//! M0 skeleton: dispatch `--selftest` early, then run the menu-bar shell.

mod app;
mod capture;
mod annotate;
mod clipboard;
mod shelf;

use app::{sandbox, selftest, delegate};

fn main() {
    // Freeze argv + env before anything reads them (Sandbox.swift).
    sandbox::init();
    // SAFETY: the entry point always runs on the process main thread; the
    // `assumeIsolated` of the Swift `main.swift`.
    let mtm = match objc2::MainThreadMarker::new() {
        Some(mtm) => mtm,
        None => panic!("Pastory must run on the main thread"),
    };
    // --selftest handling happens before anything AppKit-heavy: mutating
    // commands refuse to run against the real store and exit(2).
    if let Some(code) = selftest::try_handle_command_line() {
        std::process::exit(code);
    }

    delegate::run(mtm);
}
