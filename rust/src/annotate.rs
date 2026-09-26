//! `Annotate/` module group — one module per Swift file (M4).
//! Paths are explicit like the other groups (see app.rs).
#![allow(dead_code)]

#[path = "Annotate/annotation.rs"]
pub mod annotation;
#[path = "Annotate/renderer.rs"]
pub mod renderer;
#[path = "Annotate/text_layout.rs"]
pub mod text_layout;
#[path = "Annotate/text_view.rs"]
pub mod text_view;
#[path = "Annotate/annotate_view.rs"]
pub mod annotate_view;
#[path = "Annotate/toolbar.rs"]
pub mod toolbar;
#[path = "Annotate/ocr.rs"]
pub mod ocr;
#[path = "Annotate/ocr_panel.rs"]
pub mod ocr_panel;
