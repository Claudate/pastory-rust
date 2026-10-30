//! Port of `App/SelfTest.swift` (M0 slice: command gating + `l10n`).
//!
//! `--selftest <cmd>` · Set PASTORY_STORE=<dir> to keep test runs out of the
//! real store. Anything that writes to a store must run inside PASTORY_STORE —
//! never against the user's data.

use std::path::Path;

use crate::app::{localization, sandbox};

/// The commands that write to a store (SelfTest.swift:18). A bare read-only
/// command needs no sandbox; a mutating one refuses to run without one.
const MUTATING: &[&str] = &[
    "clipboard", "retention", "shelf", "shelfsearch", "liveshot", "search", "searchlive", "searchtype", "searchpanel", "renamepanel", "settings",
    "editors", "import", "ingest", "tombstone", "heic", "pbfiles", "welcome",
    "note", "contact", "skin", "skinlive", "settingsclick", "background", "previewvideo", "ocrlive", "ocrkeyboard",
    "contactclick", "thumbcull", "line", "linehit", "linehover", "linedrag", "linewheel", "lineexternal", "linesettings", "linegear", "lineselect", "linescroll", "lineswitch",
];

/// Dispatch `--selftest <cmd>`; returns `Some(exit code)` when handled so the
/// caller exits instead of launching the app, `None` for a normal launch.
pub fn try_handle_command_line() -> Option<i32> {
    let launch = sandbox::launch();
    let args = &launch.args;
    let i = args.iter().position(|a| a == "--selftest")?;
    if i + 1 >= args.len() {
        // A bare --selftest is a normal launch.
        return None;
    }
    let cmd = args[i + 1].as_str();

    if MUTATING.contains(&cmd) {
        // Nothing under Application Support counts as a sandbox, whatever the
        // folder is called.
        let env = sandbox::launch().store().unwrap_or_default();
        let support = dirs_application_support();
        let target = Path::new(&env).to_path_buf();
        let inside_support = support
            .as_deref()
            .map(|s| target.starts_with(s))
            .unwrap_or(false);
        if env.is_empty() || inside_support {
            println!(
                "refusing: --selftest {} needs PASTORY_STORE pointing at a scratch folder (never the real store)",
                cmd
            );
            return Some(2);
        }
    }

    let rest: Vec<String> = args[(i + 2)..].to_vec();
    let ok = run(cmd, &rest);
    Some(if ok { 0 } else { 1 })
}

fn run(cmd: &str, rest: &[String]) -> bool {
    match cmd {
        "l10n" => {
            // The table is a list of pairs on purpose: a duplicate must be a
            // test failure, never a launch crash.
            let dups = localization::duplicate_keys();
            println!(
                "{} no duplicate keys {:?}",
                if dups.is_empty() { "ok  " } else { "FAIL" },
                dups
            );
            println!("ok   {} entries", localization::PAIRS.len());
            dups.is_empty()
        }
        // M0 spikes (read-only, no store): panel + fonts + CGEvent post.
        "panel" => {
            let mtm = objc2::MainThreadMarker::new().expect("spike on main thread");
            let ok = crate::app::spike::show_brief_panel(mtm, 2.0);
            println!(
                "{} panel shown (borderless + nonactivating + shielding level)",
                if ok { "ok  " } else { "FAIL" }
            );
            ok
        }
        "fonts" => {
            let any = crate::app::theme::register_brand_fonts();
            println!(
                "{} brand fonts registered ({})",
                if any { "ok  " } else { "FAIL" },
                if any { "YsabeauOffice" } else { "none found" }
            );
            any
        }
        "paste" => {
            // CGEvent post(.cgSessionEventTap) spike (red line 3/6): post one
            // synthetic ⌘V and verify it adds no persistent latch. The check
            // is delta-based: machines can legitimately idle with modifier
            // bits held (a stuck key elsewhere shows up here too), so what
            // matters is that OUR post leaves the state as it found it —
            // exactly what the Swift production path does on this machine.
            use objc2_core_graphics as cg;
            let before =
                cg::CGEventSource::flags_state(cg::CGEventSourceStateID::CombinedSessionState);
            crate::app::permissions::send_paste();
            // The retry/post path hops through the main queue, so pump the
            // run loop while waiting; a plain sleep would starve it.
            {
                use objc2_foundation::{NSDate, NSRunLoop};
                let run_loop = NSRunLoop::mainRunLoop();
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(1200);
                while std::time::Instant::now() < deadline {
                    let limit = NSDate::dateWithTimeIntervalSinceNow(0.05);
                    // SAFETY: NSDefaultRunLoopMode is a valid mode token.
                    let mode: &'static objc2_foundation::NSRunLoopMode =
                        unsafe { objc2_foundation::NSDefaultRunLoopMode };
                    run_loop.runMode_beforeDate(mode, &limit);
                }
            }
            // The keystroke is in flight; the state can read transiently
            // while it lands, so allow up to 2 s to settle back. A true
            // latch (the 2026-09-17 HID incident) never settles — that is
            // what this catches.
            let mut after =
                cg::CGEventSource::flags_state(cg::CGEventSourceStateID::CombinedSessionState);
            let mut settled = after == before;
            if !settled {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    after = cg::CGEventSource::flags_state(
                        cg::CGEventSourceStateID::CombinedSessionState,
                    );
                    if after == before {
                        settled = true;
                        break;
                    }
                }
            }
            println!(
                "     synthetic ⌘V posted at session tap; flags before={:?} after={:?} ({})",
                before,
                after,
                if settled {
                    "settled back — matches the Swift path"
                } else {
                    "NEW LATCH — red line violated"
                }
            );
            println!(
                "{} session-tap post left modifier state as found",
                if settled { "ok  " } else { "FAIL" }
            );
            settled
        }
        // M1: the clipboard core (store / monitor / retention / writer).
        "pbtypes" => crate::app::selftest_m1::pbtypes(),
        "pbfiles" => crate::app::selftest_m1::pbfiles(rest),
        "clipboard" => crate::app::selftest_m1::clipboard(rest),
        "ingest" => crate::app::selftest_m1::ingest(),
        "retention" => crate::app::selftest_m1::retention(),
        "tombstone" => crate::app::selftest_m1::tombstone(),
        "heic" => crate::app::selftest_m1::heic(),
        // M5 adds the movie codecs to the M1 pasteboard writer.
        "writer" => {
            let mv = crate::app::selftest_m5::writer_modes();
            mv && crate::app::selftest_m1::writer()
        }
        // M2: search index + shelf model (SearchSelfTest.swift).
        "search" => crate::app::search_selftest::run(),
        // M2 UI: offscreen shelf renders (SelfTest.swift:502-523).
        "shelf" => crate::app::selftest_m2::render_shelf(
            rest.first().map(String::as_str).unwrap_or("pastory-shelf.png"),
            false,
        ),
        "shelfsearch" => crate::app::selftest_m2::render_shelf(
            rest.first().map(String::as_str).unwrap_or("/tmp/pastory-search.png"),
            true,
        ),
        // M2 UI: the live panel on screen (material composited by the window
        // server) — pair with an external screencapture.
        "liveshot" => crate::app::selftest_m2::live_shot(
            rest.first().and_then(|s| s.parse::<f64>().ok()).unwrap_or(4.0),
        ),
        // M2 UI: the search pipeline through the real view tree + poll tick
        // (the part `search` cannot see; it drives the blocking twin).
        "searchlive" => crate::app::selftest_m2::searchlive(),
        // M2 UI: the real search field + delegate — the one stage
        // `searchlive` bypasses by recording the query itself.
        "searchtype" => crate::app::selftest_m2::searchtype(),
        // M2 UI: the live ShelfPanel and a real key event through its
        // `sendEvent:` — the route `searchtype` and `searchlive` both skip.
        "searchpanel" => crate::app::selftest_m2::searchpanel(),
        "renamepanel" => crate::app::selftest_m2::renamepanel(),
        // M2 UI: drive a card's Pin button through the real click route.
        "clickpin" => crate::app::selftest_m2::clickpin(),
        // M2 UI: the 全部-tab picture flicker — the visible-band thumbnail cull
        // (warming the whole list churned the cache, so on-screen pictures
        // blinked between placeholder and image).
        "thumbcull" => crate::app::selftest_m2::thumbcull(),
        // M2 UI: the pinned-delete confirm + the poll timer that used to
        // re-enter the model borrow inside its nested event loop.
        "deletepin" => crate::app::selftest_m2::deletepin(),
        // M3 diagnostics: menu-bar click wiring (target/action/mask/performClick).
        "gif" => crate::app::selftest_m5::gif(),
        "preview" => crate::app::selftest_m5::preview(
            rest.first().map(String::as_str).unwrap_or("pastory-preview.png"),
        ),
        "statusclick" => {
            let mtm = objc2::MainThreadMarker::new().expect("selftest on main thread");
            let _ = mtm; // delegate path takes its own marker
            crate::app::delegate::statusclick_selftest()
        }
        // M4: annotation render set (canvas + toolbar + flattened export).
        "annotate" => crate::app::selftest_m4::annotate(
            rest.first().map(String::as_str).unwrap_or("pastory-annotate.png"),
        ),
        // M4: live text-box driving (synthetic NSEvent through the canvas).
        "annotationtext" => crate::app::selftest_m4::annotationtext(
            rest.first().map(String::as_str).unwrap_or("/tmp/pastory-annotationtext.png"),
        ),
        "ocrpanel" => crate::app::selftest_m4::ocrpanel(
            rest.first().map(String::as_str).unwrap_or("pastory-ocrpanel.png"),
        ),
        // M4: Vision OCR.
        "ocr" => crate::app::selftest_m4::ocr(rest.first().map(String::as_str)),
        // M4: the two OCR paths that live on a worker thread (the background
        // pass a copied screenshot gets, and the panel's own spawn+bounce) —
        // `ocr` and `ocrpanel` both skip the thread that used to panic.
        "ocrlive" => crate::app::selftest_m4::ocrlive(),
        // M4: the 识别文字 panel's keyboard route (canBecomeKeyWindow + caret),
        // which both `ocr` and `ocrlive` read past.
        "ocrkeyboard" => crate::app::selftest_m4::ocrkeyboard(),
        // M6: settings / notes / import / updater / editors / menu rows.
        "settings" => crate::app::selftest_m6::settings(
            rest.first().map(String::as_str).unwrap_or("pastory-settings.png"),
        ),
        // M7: the settings pane once per skin (the 外观 picker's own render).
        "skin" => crate::app::selftest_m6::skins(
            rest.first().map(String::as_str).unwrap_or("/tmp/pastory-skins"),
        ),
        // M7: the same switch on the live panel — instant, and no jump.
        "skinlive" => crate::app::selftest_m6::skinlive(),
        // M7: a real click inside the settings pane — the tab rail and the
        // skin picker, which `skinlive` bypasses by calling the switch itself.
        "settingsclick" => crate::app::selftest_m6::settingsclick(),
        "contact" => crate::app::selftest_m6::contact(
            rest.first().map(String::as_str).unwrap_or("pastory-contact.png"),
        ),
        // M7: a real click on the sidebar's 联系我 row of the live panel —
        // the route the offscreen `contact` render cannot take, and the one
        // the 2026-10-14/15 click-contact aborts ran.
        "contactclick" => crate::app::selftest_m6::contactclick(),
        // M7: the full-size picture viewer over a real image card.
        "viewer" => crate::app::selftest_m6::viewer(
            rest.first().map(String::as_str).unwrap_or("pastory-viewer.png"),
        ),
        // M7: the panel background's memory (pick → remember → clear), which
        // the renders above cannot show.
        // M7: a video card's 「查看」 — the click that used to abort the
        // process (the preview resolves its file from the model, so it must
        // not run inside a model borrow).
        "previewvideo" => crate::app::selftest_m6::preview_video(
            rest.first().map(String::as_str),
        ),
        "background" => crate::app::selftest_m6::background(
            rest.first()
                .map(String::as_str)
                .unwrap_or("/tmp/pastory-background"),
        ),
        "welcome" => crate::app::selftest_m6::welcome(
            rest.first().map(String::as_str).unwrap_or("pastory-welcome.png"),
        ),
        "note" => crate::app::selftest_m6::note(
            rest.first().map(String::as_str).unwrap_or("pastory-note.png"),
        ),
        "editors" => crate::app::selftest_m6::editors(
            rest.first().map(String::as_str).unwrap_or("pastory-editor.png"),
        ),
        #[cfg(not(feature = "mas"))]
        "updatewin" => crate::app::selftest_m6::updatewin(
            rest.first().map(String::as_str).unwrap_or("pastory-updatewin.png"),
        ),
        #[cfg(not(feature = "mas"))]
        "download" => crate::app::selftest_m6::download(rest),
        "updater" => crate::app::selftest_m6::updater(),
        "import" => crate::app::selftest_m6::import(rest),
        // M8: the 晾衣绳 form — the rope rendered offscreen, the swing, and
        // the click-through flip (the window behaviour no still render shows).
        "line" => crate::app::selftest_m8::line(
            rest.first().map(String::as_str).unwrap_or("pastory-line.png"),
        ),
        "linewind" => crate::app::selftest_m8::linewind(),
        "linehover" => crate::app::selftest_m8::linehover(),
        "linehit" => crate::app::selftest_m8::linehit(),
        "linedrag" => crate::app::selftest_m8::linedrag(),
        "linewheel" => crate::app::selftest_m8::linewheel(),
        "lineexternal" => crate::app::selftest_m8::lineexternal(),
        "linesettings" => crate::app::selftest_m8::linesettings(),
        "linegear" => crate::app::selftest_m8::linegear(),
        "lineselect" => crate::app::selftest_m8::lineselect(),
        "linescroll" => crate::app::selftest_m8::linescroll(),
        "lineswitch" => crate::app::selftest_m8::lineswitch(),
        "exportdir" => crate::app::selftest_m6::exportdir(),
        "openpanel" => crate::app::selftest_m6::openpanel(),
        // M3: real capture vs /usr/sbin/screencapture sampling.
        "capture" => crate::app::selftest_m3::capture(
            rest.first().map(String::as_str).unwrap_or("pastory-capture.png"),
        ),
        other => {
            println!("unknown selftest {}", other);
            false
        }
    }
}

/// `~/Library/Application Support`, symlink-resolved (Swift's
/// `resolvingSymlinksInPath` equivalent).
fn dirs_application_support() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = Path::new(&home)
        .join("Library")
        .join("Application Support");
    base.canonicalize().ok().or(Some(base))
}
