//! `Shelf/` module group — one module per Swift file, explicit paths
//! (see app.rs for why). M2 lands the model first; panel/view/card follow
//! with the UI slice.
#![allow(dead_code)]

#[path = "Shelf/model.rs"]
pub mod model;
#[path = "Shelf/card.rs"]
pub mod card;
#[path = "Shelf/view.rs"]
pub mod view;
#[path = "Shelf/panel.rs"]
pub mod panel;
#[path = "Shelf/exporter.rs"]
pub mod exporter;
#[path = "Shelf/text_editor.rs"]
pub mod text_editor;
#[path = "Shelf/image_editor.rs"]
pub mod image_editor;
#[path = "Shelf/settings_pane.rs"]
pub mod settings_pane;
#[path = "Shelf/desktop_notes.rs"]
pub mod desktop_notes;
#[path = "Shelf/desktop_note_view.rs"]
pub mod desktop_note_view;
#[path = "Shelf/how_to_card.rs"]
pub mod how_to_card;
#[path = "Shelf/welcome_card.rs"]
pub mod welcome_card;
#[path = "Shelf/contact_pane.rs"]
pub mod contact_pane;
