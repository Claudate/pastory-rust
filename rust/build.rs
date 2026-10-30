//! Bake a build stamp into the binary: short git hash, `-dirty` when the
//! worktree has uncommitted changes, and the build time.
//!
//! The release version alone cannot tell two local builds apart — every
//! rebuild of 1.0.7 ships as "1.0.7" — so when "it still behaves the old way"
//! comes up, the stamp under 设置 › 版本更新 is what says which binary is
//! actually running. Re-runs when HEAD moves.
use std::process::Command;

fn main() {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let mut code = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".into());
    if !git(&["status", "--porcelain"]).unwrap_or_default().is_empty() {
        // Uncommitted edits: the hash alone would point at the wrong source.
        code.push_str("-dirty");
    }
    println!("cargo:rustc-env=PASTORY_BUILD_STAMP={} · {}", code, build_time());
    // Re-run whenever the source, this script, or the branch tip changes: the
    // default (any package file) would miss a commit that touches nothing
    // else, and listing only `.git/HEAD` would freeze the time on a dirty
    // tree rebuilt from edited source.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}

/// Local build time, minute resolution (`YYYY-MM-DD HH:MM`).
fn build_time() -> String {
    Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}
