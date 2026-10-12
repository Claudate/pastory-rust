//! The brand: one constant for the product name everywhere user-facing
//! (bundle display name, HUD, export file prefixes, menus, updater UA).
//!
//! Fork line: the store brand is PasteJar. The upstream / transition names
//! stay in `LEGACY_BRANDS` for *matching only* — release-notes title lines and
//! the seeded how-to card written by older builds — never for new UI.

/// Current brand name.
pub const BRAND: &str = "PasteJar";

/// Names this product shipped under before. Match against stored/legacy
/// strings only; do not render these to the user.
pub const LEGACY_BRANDS: &[&str] = &["Pastory", "Pastory Rust", "Pastory-Rust"];
