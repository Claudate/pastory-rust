//! `Clipboard/` module group — one module per Swift file, explicit paths
//! (see app.rs for why). M1 is the store/monitor/retention core plus the
//! importer's Pastory-exact path (foreign formats arrive in M6).
#![allow(dead_code)]

#[path = "Clipboard/item.rs"]
pub mod item;
#[path = "Clipboard/store.rs"]
pub mod store;
#[path = "Clipboard/db.rs"]
pub mod db;
#[path = "Clipboard/retention.rs"]
pub mod retention;
#[path = "Clipboard/monitor.rs"]
pub mod monitor;
#[path = "Clipboard/search_index.rs"]
pub mod search_index;
#[path = "Clipboard/importer.rs"]
pub mod importer;
