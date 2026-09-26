//! `App/` module group — one module per Swift file (see §4 of RUST_REWRITE.md).
//!
//! The port lands milestone by milestone; M0 carries the full surface of the
//! small app-support files, whose callers arrive with later milestones, so
//! dead-code warnings are silenced until then. Paths are explicit so the
//! capitalized Swift-mirroring directory resolves on any filesystem case
//! sensitivity (Rust module roots are lowercase by convention).
#![allow(dead_code)]

#[path = "App/sandbox.rs"]
pub mod sandbox;
#[path = "App/localization.rs"]
pub mod localization;
#[path = "App/preferences.rs"]
pub mod preferences;
#[path = "App/preferences_m1.rs"]
pub mod preferences_m1;
#[path = "App/preferences_m6.rs"]
pub mod preferences_m6;
#[path = "App/selftest.rs"]
pub mod selftest;
#[path = "App/selftest_m1.rs"]
pub mod selftest_m1;
#[path = "App/search_selftest.rs"]
pub mod search_selftest;
#[path = "App/selftest_m2.rs"]
pub mod selftest_m2;
#[path = "App/selftest_m3.rs"]
pub mod selftest_m3;
#[path = "App/selftest_m4.rs"]
pub mod selftest_m4;
#[path = "App/selftest_m5.rs"]
pub mod selftest_m5;
#[path = "App/synthetic_movie.rs"]
pub mod synthetic_movie;
#[path = "App/coordinates.rs"]
pub mod coordinates;
#[path = "App/theme.rs"]
pub mod theme;
#[path = "App/hotkey.rs"]
pub mod hotkey;
#[path = "App/permissions.rs"]
pub mod permissions;
#[path = "App/delegate.rs"]
pub mod delegate;
#[path = "App/spike.rs"]
pub mod spike;
#[path = "App/updater.rs"]
pub mod updater;
#[path = "App/update_progress.rs"]
pub mod update_progress;
#[path = "App/selftest_m6.rs"]
pub mod selftest_m6;
