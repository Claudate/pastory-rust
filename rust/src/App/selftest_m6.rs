//! Port of the M6 render/command selftests (`App/SelfTest.swift`):
//! `settings` · `contact` · `welcome` · `note` · `editors` · `updatewin` ·
//! `download` · `updater` · `import` · `openpanel`.
//!
//! Render commands build the same views the live path uses, hosted in a
//! borderless window at the Swift frames, and snap to PNG through the
//! shared `selftest_m2::snapshot`.


use objc2::MainThreadMarker;
use objc2_app_kit::{NSBackingStoreType, NSOpenPanel, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDate, NSRunLoop, NSString, NSURL};

use crate::app::{sandbox, selftest_m2};
use crate::clipboard::item::ClipKind;
use crate::clipboard::store::{self};
use crate::shelf::panel;

/// The shelf host pipeline every pane render shares (SelfTest's settings /
/// contact / welcome blocks all build the same tree and snap).
fn render_shelf_host(lean_reset: impl FnOnce(&'_ mut crate::shelf::model::ShelfModel), height: f64, out: &str) -> bool {
    selftest_m2::seed_store_path();
    panel::with_model_mut(lean_reset);
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    // `PASTORY_WIDTH` mirrors the Swift render command's host width override.
    let width = sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_WIDTH")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(1600.0);
    let size = CGSize::new(width, height);
    let host = crate::shelf::view::build(mtm, size);
    host.setFrame(CGRect::new(CGPoint::ZERO, size));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, size),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&host));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    selftest_m2::snapshot(&host, out)
}

/// `PASTORY_HEIGHT` = the host window's height, keeping step with the Swift render commands.
fn env_height(default: f64) -> f64 {
    sandbox::launch()
        .env
        .iter()
        .find(|(k, _)| k == "PASTORY_HEIGHT")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

pub fn settings(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_settings = true;
    }, env_height(450.0), out)
}

pub fn contact(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_contact = true;
    }, 560.0, out)
}

pub fn welcome(out: &str) -> bool {
    render_shelf_host(|m| {
        m.reset();
        m.show_welcome = true;
    }, env_height(450.0), out)
}

/// `SelfTest.note` — the first text card rendered as a desktop note
/// (natural size branch against the store's own content).
pub fn note(out: &str) -> bool {
    selftest_m2::seed_store_path();
    let Some(item) = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text).cloned()) else {
        println!("note: no text item");
        return false;
    };
    crate::shelf::desktop_notes::place(&item.id, Some(CGPoint::new(80.0, 200.0)));
    let Some(view) = crate::shelf::desktop_notes::any_note_view() else {
        println!("note: no view");
        return false;
    };
    let h = crate::shelf::desktop_note_view::NoteView::natural_height(&item) + crate::shelf::desktop_note_view::NOTE_MARGIN * 2.0;
    let w = crate::shelf::desktop_note_view::NOTE_W + crate::shelf::desktop_note_view::NOTE_MARGIN * 2.0;
    view.setFrame(CGRect::new(CGPoint::ZERO, CGSize::new(w, h)));
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc::<NSWindow>(),
            CGRect::new(CGPoint::ZERO, CGSize::new(w, h)),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setContentView(Some(&view));
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    let ok = selftest_m2::snapshot(&view, out);
    crate::shelf::desktop_notes::close(&item.id);
    ok
}

/// `SelfTest.editors` — three renders: titled text, untitled text, image.
pub fn editors(out: &str) -> bool {
    selftest_m2::seed_store_path();
    let base = out.strip_suffix(".png").unwrap_or(out).to_string();
    let mut ok = true;
    let titled = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text && it.title.is_some()).cloned());
    if let Some(t) = titled {
        match crate::shelf::text_editor::TextEditorWindow::debug_view(&t) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.text.png")) && ok; }
            None => ok = false,
        }
    }
    let untitled = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Text && it.title.is_none()).cloned());
    if let Some(t) = untitled {
        match crate::shelf::text_editor::TextEditorWindow::debug_view(&t) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.text-untitled.png")) && ok; }
            None => ok = false,
        }
    }
    let image = store::read(|s| s.items.iter().find(|it| it.kind == ClipKind::Image).cloned());
    if let Some(im) = image {
        match crate::shelf::image_editor::ImageEditorWindow::debug_view(&im) {
            Some(v) => { ok = selftest_m2::snapshot(&v, &format!("{base}.image.png")) && ok; }
            None => ok = false,
        }
    }
    ok
}

/// `SelfTest.updatewin` — the progress panel at 1.9 / 4.86 MB (Swift:
/// `UpdateProgressWindow(version:)` + `update(done:total:)` + frame 380×150).
pub fn updatewin(out: &str) -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let w = crate::app::update_progress::Window::new("1.0.3".into());
    w.update(1_900_000, 4_860_000);
    let content = w.content_view();
    content.setFrame(CGRect::new(CGPoint::ZERO, content.frame().size));
    content.layoutSubtreeIfNeeded();
    selftest_m2::snapshot(&content, out)
}

/// `SelfTest.download <url> [out]` — the downloader exercised for real.
pub fn download(rest: &[String]) -> bool {
    let url_s = rest.first().map(String::as_str).unwrap_or("");
    let dest = rest
        .get(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pastory-dl-test"));
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url_s)) else {
        println!("download: bad url {url_s}");
        return false;
    };
    match crate::app::update_progress::fetch_blocking(&url, &dest) {
        Ok(bytes) => {
            println!("ok   {bytes} bytes");
            true
        }
        Err(e) => {
            println!("failed: {}", e.describe());
            false
        }
    }
}

/// `SelfTest.updater` — fixture parse + version compare + notes strip +
/// the ad-hoc self-install guard (Swift's checks table).
pub fn updater() -> bool {
    let fixture = r#"{"tag_name":"v1.2","html_url":"https://github.com/nothingbutcici/pastory/releases/tag/v1.2","body":"- faster shelf\n- HEIC option",
  "assets":[{"name":"Pastory-1.2.zip","browser_download_url":"https://github.com/nothingbutcici/pastory/releases/download/v1.2/Pastory-1.2.zip"}]}"#;
    let r = crate::app::updater::parse_release(fixture.as_bytes()).ok();
    if let Some(rel) = &r {
        println!(
            "parsed: {} {} notes={}",
            rel.version,
            rel.zip_url.as_deref().and_then(|u| u.rsplit('/').next()).unwrap_or("-"),
            rel.notes.len()
        );
    }
    let checks: Vec<(&str, bool)> = vec![
        ("parses tag/notes/asset", r.as_ref().map(|rel| {
            rel.version == "1.2" && rel.zip_url.as_deref().and_then(|u| u.rsplit('/').next()) == Some("Pastory-1.2.zip") && rel.notes.contains("HEIC")
        }).unwrap_or(false)),
        ("1.2 > 1.0", crate::app::updater::is_newer("1.2", "1.0")),
        ("1.0.1 > 1.0", crate::app::updater::is_newer("1.0.1", "1.0")),
        ("1.0 !> 1.0", !crate::app::updater::is_newer("1.0", "1.0")),
        ("1.10 > 1.9", crate::app::updater::is_newer("1.10", "1.9")),
        ("notes: zh half, no markdown", {
            let t = crate::app::updater::notes_for_display(
                "Pastory 1.2\n\n**新功能**\n- 一\n- 二\n\n---\n\nPastory 1.2\n\n**New**\n- one",
                "1.2",
                false,
            );
            t == "新功能\n•  一\n•  二"
        }),
        ("notes: en half", crate::app::updater::notes_for_display("中文\n---\n**New**\n- one", "1.2", true) == "New\n•  one"),
        ("notes: no divider keeps all", crate::app::updater::notes_for_display("- a\n- b", "1.2", true) == "•  a\n•  b"),
        ("ad-hoc build must not self-install", {
            // The dev build of this binary is ad-hoc-signed: it must never
            // replace itself (Swift checks teamIdentifier==nil → canSelfInstall false).
            let bundle = crate::app::updater::bundle_path();
            let team = crate::app::updater::team_identifier(&bundle);
            team.is_none() && !crate::app::updater::can_self_install() || team.is_some()
        }),
    ];
    let mut ok = true;
    for (name, good) in &checks {
        println!("{} {}", if *good { "ok  " } else { "FAIL" }, name);
        ok &= *good;
    }
    ok
}

/// `SelfTest.import <db>` — scan a foreign SQLite and pull it into the
/// sandbox store (prints what it found; asserted n > 0 like Swift).
pub fn import(rest: &[String]) -> bool {
    let Some(db_path) = rest.first() else {
        println!("import needs a db path");
        return false;
    };
    let path = std::path::PathBuf::from(db_path);
    match crate::clipboard::importer::scan(&path) {
        Ok(scan) => {
            println!(
                "scan: {} texts, {} images from tables {:?}",
                scan.texts(),
                scan.images(),
                scan.tables
            );
            for e in scan.entries.iter().take(12) {
                match &e.payload {
                    crate::clipboard::store::ImportPayload::Text(t) => println!(
                        "  text  {} pin={} {}",
                        e.created_at,
                        e.pinned,
                        t.chars().take(40).collect::<String>().replace('\n', "⏎")
                    ),
                    crate::clipboard::store::ImportPayload::Image(d) => println!(
                        "  image {} pin={} {} bytes",
                        e.created_at,
                        e.pinned,
                        d.len()
                    ),
                }
            }
            let n = store::with(|s| s.import_entries(scan.entries));
            println!("imported {n}; store now {}", store::read(|s| s.items.len()));
            n > 0
        }
        Err(f) => {
            println!("import failed: {}", f.message());
            false
        }
    }
}

/// `SelfTest.openpanel` — how long until the system open panel is actually
/// on screen (Swift's first-show timing).
pub fn openpanel() -> bool {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let t0 = std::time::Instant::now();
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(true);
    let home = std::env::var_os("HOME").expect("HOME");
    let url = NSURL::fileURLWithPath(&NSString::from_str(&home.to_string_lossy()));
    panel.setDirectoryURL(Some(&url));
    unsafe {
        let _: () = objc2::msg_send![&app, activate];
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let block = block2::RcBlock::new(|_response: isize| {});
    unsafe {
        let _: () = objc2::msg_send![&*panel, beginWithCompletionHandler: &*block];
    }
    let mut waited = 0.0f64;
    let rl = NSRunLoop::mainRunLoop();
    let visible = loop {
        if panel.isVisible() { break true; }
        if waited >= 30.0 { break panel.isVisible(); }
        let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
        let mode: &'static objc2_foundation::NSRunLoopMode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
        let _ = rl.runMode_beforeDate(mode, &limit);
        waited += 0.05;
    };
    println!(
        "open panel visible after {:.2} s (visible={})",
        t0.elapsed().as_secs_f64(),
        if visible { "yes" } else { "no" }
    );
    // Close the sheet (`panel.cancel(nil)`); raw msg_send sidesteps the
    // generated binding's typing of the sender.
    unsafe {
        let nil_sender: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
        let _: () = objc2::msg_send![&*panel, cancel: nil_sender];
    }
    visible || waited < 30.0
}

/// Keeps the host alive one window beat, like the Swift tests' windows.
#[allow(dead_code)]
fn host_keep(_: &NSWindow) {}
