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
    // Resolve the skin before anything draws (the menu-bar shell's prewarm
    // builds the panel, and every colour, radius and face comes from here).
    app::theme_skin::init();
    // Resolve the shelf's form for this process, next to the skin: the shell
    // prewarm builds the panel, and its window and tree depend on which form
    // is in force.
    app::layout_skin::init();
    // SAFETY: the entry point always runs on the process main thread; the
    // `assumeIsolated` of the Swift `main.swift`.
    let mtm = match objc2::MainThreadMarker::new() {
        Some(mtm) => mtm,
        None => panic!("PasteJar must run on the main thread"),
    };
    // --selftest handling happens before anything AppKit-heavy: mutating
    // commands refuse to run against the real store and exit(2).
    if let Some(code) = selftest::try_handle_command_line() {
        std::process::exit(code);
    }

    delegate::run(mtm);
}
