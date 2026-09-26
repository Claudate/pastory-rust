//! `Capture/` module group — one module per Swift file, explicit paths (see
//! app.rs for why). M1 carries the codec half of Screenshotter and the
//! pasteboard writer; the ScreenCaptureKit capture paths land in M3.
#![allow(dead_code)]

#[path = "Capture/screenshotter.rs"]
pub mod screenshotter;
#[path = "Capture/pasteboard_writer.rs"]
pub mod pasteboard_writer;
#[path = "Capture/target.rs"]
pub mod target;
#[path = "Capture/selection_overlay.rs"]
pub mod selection_overlay;
#[path = "Capture/coordinator.rs"]
pub mod coordinator;
#[path = "Capture/screen_recorder.rs"]
pub mod screen_recorder;
#[path = "Capture/gif_encoder.rs"]
pub mod gif_encoder;
#[path = "Capture/recording_preview.rs"]
pub mod recording_preview;
#[path = "Capture/recording_session.rs"]
pub mod recording_session;
