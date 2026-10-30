//! M8 self-tests: the 晾衣绳 form (`--selftest line` / `linewind` / `linehit`).
//!
//! Three checks, one per thing the second form can get wrong on its own:
//!
//! * `line` — the rope and the row of photos, rendered offscreen. Needs no
//!   window server, so it runs anywhere; the PNG is what a human eyeballs and
//!   what a pixel diff catches.
//! * `linewind` — the swing. A pure check of `app::motion`: a hanging photo
//!   starts tilted, comes to rest, and two cards swing apart. No window.
//! * `linehit` — the click-through flip. Builds the strip, then asserts the
//!   pointer inside each photo's rect makes the window clickable and the space
//!   past the row leaves it transparent to clicks. This is the piece the
//!   offscreen render cannot show, because it is the *window's* behaviour.
//!
//! All three run inside `PASTORY_STORE`; `line` and `linehit` seed the store
//! the way the panel renders do, so the row has pictures in it.

use objc2::MainThreadMarker;
use objc2_core_foundation::CGSize;

use crate::app::{layout_skin, selftest_m2};
use crate::shelf::line_panel;

/// `--selftest line [out.png]` — the rope rendered offscreen, right now.
///
/// The strip's geometry is resolved against the pointer's screen, so this
/// renders at a fixed desktop-ish size rather than the live screen's: the
/// point is the rope and the cards, not the window's own placement.
pub fn line(out: &str) -> bool {
    selftest_m2::seed_store_path();
    // Same prep the panel renders do: a clean model, then the picture cards'
    // thumbnails decoded and waited for, so the rope draws pictures rather
    // than placeholder wells.
    crate::shelf::panel::with_model_mut(|m| m.reset());
    selftest_m2::warm_card_thumbnails();
    // The strip form is the one under test, whatever the user's stored pick is.
    layout_skin::select("line");
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let width = crate::app::sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_WIDTH")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(1600.0);
    line_panel::render_offscreen(CGSize::new(width, line_panel::strip_height()), out)
}

/// `--selftest linewind` — the swing at three moments.
pub fn linewind() -> bool {
    line_panel::selftest_wind_report()
}

/// `--selftest linehover` — the automatic reveal and retract: the pointer at
/// the menu bar brings the strip out; away from it, the strip goes.
pub fn linehover() -> bool {
    selftest_m2::seed_store_path();
    layout_skin::select("line");
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    line_panel::prewarm();
    line_panel::selftest_hover_report()
}

/// `--selftest linedrag` — the drag gate: a drag in flight must keep the strip
/// on screen and clickable; ending it hands control back to the clock.
pub fn linedrag() -> bool {
    prep_strip();
    line_panel::selftest_drag_report()
}

/// `--selftest lineexternal` — the external-window gate: while one of our
/// editors / the viewer / Quick Look holds the foreground, the strip neither
/// reveals nor retracts; when it closes, the strip gets a grace before the
/// clock may take it away again.
pub fn lineexternal() -> bool {
    prep_strip();
    line_panel::selftest_external_report()
}

/// `--selftest linesettings` — the settings page opened from the strip: the
/// form switch the page needs must not be written down as the user's choice,
/// and leaving the page must hand the strip back.
pub fn linesettings() -> bool {
    prep_strip();
    line_panel::selftest_settings_report()
}

/// `--selftest linegear` — the empty strip's settings hot zone: it takes the
/// click, the rest of the strip still lets clicks through, and a rope with
/// photos on it offers no zone at all.
pub fn linegear() -> bool {
    prep_strip();
    line_panel::selftest_gear_report()
}

/// `--selftest lineselect` — the rope's selected ring: 编辑 marks the card it
/// acted on while the green check stays on the shelf's first clip, and the two
/// offscreen renders differ in exactly those two cards.
pub fn lineselect() -> bool {
    prep_strip();
    line_panel::selftest_selection_report()
}

/// `--selftest linescroll` — the row's scroll position across a rebuild (the
/// 「改完卡片大小，绳子回到第一张」 defect).
pub fn linescroll() -> bool {
    prep_strip();
    line_panel::selftest_scroll_report()
}

/// `--selftest lineswitch` — picking 晾衣绳 in 设置 › 外观 › 形态 takes the drawer
/// down. The page lives on the drawer's own window, so a switch that only
/// raises the strip leaves the user staring at the page they just left.
///
/// Deliberately *not* `prep_strip`: this test has to start from the drawer form
/// with the settings page open, which is where the defect showed.
pub fn lineswitch() -> bool {
    selftest_m2::seed_store_path();
    line_panel::selftest_form_switch_report()
}

/// `--selftest linewheel` — the strip's horizontal wheel, and the document
/// width that gives it something to move.
pub fn linewheel() -> bool {
    prep_strip();
    line_panel::selftest_wheel_report()
}

/// Shared setup for the strip's live self-tests: a seeded store, the line form
/// selected, an accessory app so a window can be ordered in, and the strip
/// prewarmed and shown (the clock frozen so it cannot retract it underfoot).
fn prep_strip() {
    selftest_m2::seed_store_path();
    layout_skin::select("line");
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    line_panel::prewarm();
    line_panel::debug_freeze_auto();
    line_panel::show();
    line_panel::debug_settle(true);
}

/// `--selftest linehit` — the click-through flip, against the live strip.
pub fn linehit() -> bool {
    // The strip never activates the app, so no `activate_app`; but the window
    // has to exist and be shown before the flip means anything, and the clock
    // frozen so it cannot retract the strip the moment the test looks away.
    prep_strip();
    line_panel::selftest_hit_report()
}
