//! Port of `App/Updater.swift` — GitHub Releases check, once a day and on
//! demand. The only network request Pastory ever makes; it carries no
//! identifier (settings can turn it off).
//!
//! Networking runs through NSURLSession (objc2-foundation), matching the
//! Swift implementation's delegate/session behaviour. Pure helpers (parse,
//! isNewer, notesForDisplay, teamIdentifier, canSelfInstall) are sync so the
//! `updater` self-test runs without network; the interactive flow hops back
//! to the main queue like Swift's `@MainActor`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSAlert, NSApplication, NSWorkspace};
use objc2_foundation::{
    ns_string, NSArray, NSData, NSDictionary, NSError, NSHTTPURLResponse, NSJSONReadingOptions,
    NSJSONSerialization, NSMutableURLRequest, NSString, NSURL, NSURLSession,
};

use crate::app::localization::l;
use crate::app::preferences::Preferences;

pub const REPO: &str = "nothingbutcici/pastory";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub zip_url: Option<String>,
    pub page: String,
}

/// Current version, from the bundle.
pub fn current_version() -> String {
    let bundle = objc2_foundation::NSBundle::mainBundle();
    bundle
        .objectForInfoDictionaryKey(ns_string!("CFBundleShortVersionString"))
        .and_then(|v| {
            let s: &objc2_foundation::NSString = unsafe {
                &*(objc2::rc::Retained::as_ptr(&v) as *const objc2_foundation::NSString)
            };
            if s.class().name().to_bytes().starts_with(b"NSString") || s.isKindOfClass(objc2::class!(NSString)) {
                Some(s.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "0".into())
}

/// The running `.app` bundle's path: `<x>.app/Contents/MacOS/pastory` → the
/// `.app` (the `updatewin`/`download` self-tests run the bare binary but never
/// reach this).
pub fn bundle_path() -> PathBuf {
    let exe = std::env::current_exe().expect("exe path");
    exe.ancestors()
        .nth(2)
        .filter(|a| a.extension().map(|e| e == "app").unwrap_or(false))
        .map(Path::to_path_buf)
        .unwrap_or(exe)
}

// MARK: Pure helpers (identical semantics to the Swift originals)

/// "1.2.1" > "1.2" > "1.0"; non-numeric parts compare as 0.
pub fn is_newer(a: &str, than_b: &str) -> bool {
    let pa: Vec<i64> = a.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    let pb: Vec<i64> = than_b.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = *pa.get(i).unwrap_or(&0);
        let y = *pb.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Release notes are Markdown with the Chinese half first and the English
/// half after a `---` line. Pick the half for the UI language, drop the
/// "Pastory x.y" title line the alert already carries, turn bold/headings/
/// list dashes into plain text and bullets, and cap the length.
pub fn notes_for_display(raw: &str, version: &str, english: bool) -> String {
    let halves: Vec<&str> = raw.split("\n---\n").collect();
    let text = if halves.len() >= 2 {
        if english {
            halves[1..].join("\n")
        } else {
            halves[0].to_string()
        }
    } else {
        raw.to_string()
    };
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let mut trimmed = line.trim().to_string();
        if trimmed == format!("Pastory {version}") || trimmed == "---" {
            continue;
        }
        trimmed = trimmed.replace("**", "");
        while trimmed.starts_with('#') {
            trimmed.remove(0);
        }
        trimmed = trimmed.trim().to_string();
        if let Some(rest) = trimmed.strip_prefix("- ") {
            trimmed = format!("•  {rest}");
        }
        lines.push(trimmed);
    }
    let mut text = lines.join("\n");
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    let text = text.trim().to_string();
    if text.len() > 900 {
        // UIBreak on a char boundary: 900 “characters” in Swift is a
        // character count; find the nearest boundary ≤ 900 bytes.
        let mut end = 900;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("1.2", "1.0"));
        assert!(is_newer("1.0.1", "1.0"));
        assert!(!is_newer("1.0", "1.0"));
        assert!(is_newer("1.10", "1.9"));
    }

    #[test]
    fn notes_half_strip() {
        let raw = "Pastory 1.2\n\n**新功能**\n- 一\n- 二\n\n---\n\nPastory 1.2\n\n**New**\n- one";
        let zh = notes_for_display(raw, "1.2", false);
        assert_eq!(zh, "新功能\n•  一\n•  二");
        let en = notes_for_display("中文\n---\n**New**\n- one", "1.2", true);
        assert_eq!(en, "New\n•  one");
        let no_div = notes_for_display("- a\n- b", "1.2", true);
        assert_eq!(no_div, "•  a\n•  b");
    }
}

// MARK: JSON parse

/// GitHub release JSON → `Release` (`Updater.swift:81`), via NSJSONSerialization
/// so the read semantics (any-value tree) match Foundation exactly.
pub fn parse_release(data: &[u8]) -> Result<Release, String> {
    let nsdata = NSData::from_vec(data.to_vec());
    let obj = NSJSONSerialization::JSONObjectWithData_options_error(
        &nsdata,
        NSJSONReadingOptions::empty(),
    )
    .map_err(|e| e.localizedDescription().to_string())?;
    let dict: Retained<NSDictionary<NSString, AnyObject>> =
        unsafe { Retained::cast_unchecked(obj) };
    let tag = { dict.objectForKey(ns_string!("tag_name")) }
        .map(|v| unsafe { Retained::cast_unchecked::<NSString>(v) }.to_string());
    let Some(tag) = tag else {
        return Err("cannotParseResponse: no tag_name".into());
    };
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    let zip_url = { dict.objectForKey(ns_string!("assets")) }
        .map(|v| unsafe { Retained::cast_unchecked::<NSArray<NSDictionary<NSString, AnyObject>>>(v) })
        .into_iter()
        .flat_map(|v| v.iter().collect::<Vec<_>>())
        .find_map(|a| {
            let name = { a.objectForKey(ns_string!("name")) }
                .map(|n| unsafe { Retained::cast_unchecked::<NSString>(n) }.to_string())?;
            if !name.ends_with(".zip") {
                return None;
            }
            let url_s = { a.objectForKey(ns_string!("browser_download_url")) }
                .map(|u| unsafe { Retained::cast_unchecked::<NSString>(u) }.to_string())?;
            // Only ever fetch an installer over https from GitHub's own hosts.
            let url = NSURL::URLWithString(&NSString::from_str(&url_s))?;
            let scheme = url.scheme().map(|s| s.to_string());
            let host = url.host().map(|s| s.to_string());
            match (scheme.as_deref(), host.as_deref()) {
                (Some("https"), Some(h))
                    if h == "github.com"
                        || h.ends_with(".github.com")
                        || h.ends_with(".githubusercontent.com") =>
                {
                    Some(url_s)
                }
                _ => None,
            }
        });
    let page = { dict.objectForKey(ns_string!("html_url")) }
        .map(|u| unsafe { Retained::cast_unchecked::<NSString>(u) }.to_string())
        .unwrap_or_else(|| format!("https://github.com/{REPO}/releases/latest"));
    let notes = { dict.objectForKey(ns_string!("body")) }
        .map(|u| unsafe { Retained::cast_unchecked::<NSString>(u) }.to_string())
        .unwrap_or_default();
    Ok(Release {
        version,
        notes,
        zip_url,
        page,
    })
}

// MARK: Codesign

/// `teamIdentifier(of:)` — one codesign call; None when the build is ad-hoc
/// or unsigned ("not set").
pub fn team_identifier(app: &Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2", &app.to_string_lossy()])
        .output()
        .ok()?;
    // TeamIdentifier lives on stderr; merge the streams like Swift's Pipe
    // (which only reads stderr+stdout together).
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("TeamIdentifier=") {
            let rest = rest.trim();
            if rest != "not set" && !rest.is_empty() {
                return Some(rest.to_string());
            }
            return None;
        }
    }
    None
}

/// Only a signed, notarised build knows who it is; an ad-hoc one must not
/// replace itself with an unverifiable download.
pub fn can_self_install() -> bool {
    let bundle = bundle_path();
    let Some(team) = team_identifier(&bundle) else {
        return false;
    };
    if team.is_empty() {
        return false;
    }
    let path = bundle.to_string_lossy().into_owned();
    if path.contains("/AppTranslocation/") {
        return false;
    }
    bundle
        .parent()
        .map(|p| std::fs::metadata(p).map(|m| !m.permissions().readonly()).unwrap_or(false))
        .unwrap_or(false)
}

// MARK: Scheduling and checks

/// Launch: first check after half a minute, then every 24 h while running.
pub fn schedule() {
    crate::app::delegate::dispatch_main_after(30.0, Box::new(|| {
        check(false, false);
        // 24 h loop, re-armed after every check (Timer repeats: true).
        crate::app::delegate::dispatch_main_after(24.0 * 3600.0, Box::new(schedule_periodic));
    }));
}

fn schedule_periodic() {
    check(false, false);
    crate::app::delegate::dispatch_main_after(24.0 * 3600.0, Box::new(schedule_periodic));
}

fn busy() -> &'static Mutex<bool> {
    static BUSY: OnceLock<Mutex<bool>> = OnceLock::new();
    BUSY.get_or_init(|| Mutex::new(false))
}

fn now_ts() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// What the caller shows for an interactive check (Swift's `Outcome`).
pub enum Outcome {
    UpToDate,
    Available(String),
    Failed(String),
    Skipped,
}

/// `check(interactive:quiet:)` — completion arrives on the main queue
/// (NSURLSession's handler queue hops like Swift's `await`).
pub fn check_with(interactive: bool, quiet: bool, done: impl FnOnce(Outcome) + Send + 'static) {
    if !interactive {
        if !Preferences::shared().check_for_updates() {
            done(Outcome::Skipped);
            return;
        }
        if let Some(last) = Preferences::shared().last_update_check() {
            if now_ts() - last < 20.0 * 3600.0 {
                done(Outcome::Skipped);
                return;
            }
        }
    }
    {
        let mut b = busy().lock().expect("busy poisoned");
        if *b {
            done(Outcome::Skipped);
            return;
        }
        *b = true;
    }
    Preferences::shared().set_last_update_check(now_ts());
    fetch_latest(move |outcome| {
        *busy().lock().expect("busy poisoned") = false;
        match outcome {
            Ok(release) => {
                if !is_newer(&release.version, &current_version()) {
                    if interactive && !quiet {
                        info(
                            &l("已经是最新版本"),
                            &l("Pastory %@").replacen("%@", &current_version(), 1),
                        );
                    }
                    done(Outcome::UpToDate);
                    return;
                }
                if !interactive
                    && Preferences::shared().skipped_version().as_deref() == Some(release.version.as_str())
                {
                    done(Outcome::Skipped);
                    return;
                }
                let version = release.version.clone();
                offer(release);
                done(Outcome::Available(version));
            }
            Err(msg) => {
                if interactive && !quiet {
                    info(&l("检查更新失败"), &describe_error(&msg, true));
                }
                done(Outcome::Failed(describe_error(&msg, false)));
            }
        }
    });
}

pub fn check(interactive: bool, quiet: bool) {
    check_with(interactive, quiet, |_| {});
}

/// The only HTTP GET of the app. 15 s request timeout, no identifier beyond
/// the version in the User-Agent (matches Swift).
pub fn fetch_latest(done: impl FnOnce(Result<Release, String>) + Send + 'static) {
    let url = NSURL::URLWithString(&NSString::from_str(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    )))
    .expect("literal URL");
    let req_alloc: *mut NSMutableURLRequest = unsafe { msg_send![objc2::class!(NSMutableURLRequest), alloc] };
    let req: Retained<NSMutableURLRequest> = unsafe {
        let built: *mut NSMutableURLRequest = msg_send![req_alloc, initWithURL: &*url];
        Retained::from_raw(built).expect("request built")
    };
    unsafe {
        // SAFETY: NSString header values on the standard request interface.
        let accept = NSString::from_str("application/vnd.github+json");
        let ua = NSString::from_str(&format!("Pastory/{}", current_version()));
        let k1 = NSString::from_str("Accept");
        let k2 = NSString::from_str("User-Agent");
        let _: () = msg_send![&*req, setValue: &*accept, forHTTPHeaderField: &*k1];
        let _: () = msg_send![&*req, setValue: &*ua, forHTTPHeaderField: &*k2];
    }
    req.setTimeoutInterval(15.0);
    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(done)));
    let block = RcBlock::new(
        move |data: *mut NSData, response: *mut objc2_foundation::NSURLResponse, error: *mut NSError| {
            let Some(done) = done.borrow_mut().take() else { return };
            if !error.is_null() {
                let e = unsafe { &*error };
                done(Err(e.localizedDescription().to_string()));
                return;
            }
            if data.is_null() {
                done(Err("no data".into()));
                return;
            }
            let bytes = unsafe { (&*data).as_bytes_unchecked() };
            if let Some(resp) = unsafe { response.as_ref() } {
                let http: &NSHTTPURLResponse =
                    unsafe { &*(resp as *const objc2_foundation::NSURLResponse as *const NSHTTPURLResponse) };
                let code = http.statusCode();
                if code == 404 {
                    done(Err(l("暂无发布版本")));
                    return;
                }
                if code != 200 {
                    done(Err("badServerResponse".into()));
                    return;
                }
            }
            done(parse_release(bytes));
        },
    );
    unsafe {
        let session = NSURLSession::sharedSession();
        let task = session.dataTaskWithRequest_completionHandler(&req, &block);
        task.resume();
    }
}

// MARK: Alerts

fn offer(r: Release) {
    crate::app::delegate::dispatch_main_async(Box::new(move || {
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(
            &l("Pastory %@ 可以更新了（当前 %@）")
                .replacen("%@", &r.version, 1)
                .replacen("%@", &current_version(), 1),
        ));
        alert.setInformativeText(&NSString::from_str(&notes_for_display(
            &r.notes,
            &r.version,
            crate::app::localization::is_english(),
        )));
        let can_install = r.zip_url.is_some() && can_self_install();
        alert.addButtonWithTitle(&NSString::from_str(if can_install {
            "下载并安装"
        } else {
            "打开下载页"
        }));
        alert.addButtonWithTitle(&NSString::from_str(&l("稍后")));
        alert.addButtonWithTitle(&NSString::from_str(&l("跳过这个版本")));
        activate();
        let response = alert.runModal();
        if response == objc2_app_kit::NSAlertFirstButtonReturn {
            if can_install {
                if let Some(zip) = r.zip_url.clone() {
                    install(zip, r.version.clone(), r.page.clone());
                }
            } else if let Some(url) = NSURL::URLWithString(&NSString::from_str(&r.page)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        } else if response == objc2_app_kit::NSAlertThirdButtonReturn {
            Preferences::shared().set_skipped_version(&r.version);
        }
    }));
}

fn info(title: &str, text: &str) {
    let (title, text) = (title.to_string(), text.to_string());
    crate::app::delegate::dispatch_main_async(Box::new(move || {
        let mtm = MainThreadMarker::new().expect("main thread");
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&title));
        alert.setInformativeText(&NSString::from_str(&text));
        activate();
        alert.runModal();
    }));
}

fn activate() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        let _: () = msg_send![&app, activate];
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// What went wrong, in words the user can act on. Network trouble reaching
/// GitHub is by far the usual cause.
pub fn describe_error(detail: &str, long: bool) -> String {
    match detail {
        "signature" => l("下载的更新包没有通过签名校验，已放弃安装。"),
        "archive" => l("下载的更新包不完整。"),
        s if l("暂无发布版本") == s => s.to_string(),
        _ if long => l(
            "连不上 GitHub，更新包没有下载下来。\n\n部分网络直连 GitHub 不稳定：可以打开代理后重试，或者从下载页手动下载。",
        ),
        _ => l("连不上 GitHub（可能需要代理）"),
    }
}

// MARK: Install

#[derive(Debug)]
pub enum InstallError {
    Signature,
    Archive,
    Other(String),
    Cancelled,
}

/// The download + verify + swap pipeline (`Updater.install`).
pub fn install(zip_url: String, version: String, page: String) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(&zip_url)) else { return };
    let window = crate::app::update_progress::Window::new(version.clone());
    window.show();
    // The handler hops back and forth: cross-thread with a raw panel handle
    // twice over (window → worker dispatch + worker → main dispatch), then
    // drives the UI on the main thread (Swift's window.close() hops).
    let win_raw = window.raw_pub();
    crate::app::update_progress::download(&url, window, move |result| move_to_worker(win_raw, result, version.clone(), page.clone()));
}

fn move_to_worker(
    win_raw: usize,
    result: Result<PathBuf, crate::app::update_progress::DownloadError>,
    version: String,
    page: String,
) {
    // SAFETY: the panel is a process-lifetime object; the handle is rebuilt
    // locally for the UI calls below.
    let win = unsafe { crate::app::update_progress::Window::from_raw_pub(win_raw) };
    match result {
        Ok(zip_path) => {
            win.begin_installing();
            std::thread::spawn(move || {
                let outcome = verify_and_install(&zip_path);
                let zip_path_c = zip_path.clone();
                crate::app::delegate::dispatch_main_async(Box::new(move || {
                    let win = unsafe { crate::app::update_progress::Window::from_raw_pub(win_raw) };
                    win.close();
                    let _ = std::fs::remove_file(&zip_path_c);
                    match outcome {
                        Ok(()) => relaunch(),
                        Err(e) => retry_or_open_page(version, page, e),
                    }
                }));
            });
        }
        Err(crate::app::update_progress::DownloadError::Cancelled) => win.close(),
        Err(e) => {
            win.close();
            retry_or_open_page(version, page, InstallError::Other(e.describe()));
        }
    }
}

/// Re-download the release zip and swap again (the alert's 重试 path re-finds
/// the URL through a fresh check, like Swift's recursive install).
fn retry_or_open_page(version: String, page: String, e: InstallError) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&l("自动更新没有成功")));
    let detail = match &e {
        InstallError::Signature => l("下载的更新包没有通过签名校验，已放弃安装。"),
        InstallError::Archive => l("下载的更新包不完整。"),
        InstallError::Other(s) => describe_error(s, true),
        InstallError::Cancelled => return,
    };
    alert.setInformativeText(&NSString::from_str(&detail));
    alert.addButtonWithTitle(&NSString::from_str(&l("重试")));
    alert.addButtonWithTitle(&NSString::from_str(&l("打开下载页")));
    alert.addButtonWithTitle(&NSString::from_str(&l("取消")));
    activate();
    let r = alert.runModal();
    if r == objc2_app_kit::NSAlertFirstButtonReturn {
        // Find the zip again through a fresh check rather than replaying a stale URL.
        check_quiet_reinstall(version);
    } else if r == objc2_app_kit::NSAlertSecondButtonReturn {
        if let Some(u) = NSURL::URLWithString(&NSString::from_str(&page)) {
            NSWorkspace::sharedWorkspace().openURL(&u);
        }
    }
}

fn check_quiet_reinstall(version: String) {
    fetch_latest(move |outcome| {
        if let Ok(release) = outcome {
            if release.version == version {
                if let Some(zip) = release.zip_url.clone() {
                    install(zip, release.version, release.page);
                }
            }
        }
    });
}

/// After a successful swap: relaunch the app at the same bundle path.
fn relaunch() {
    let target = bundle_path();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&target.to_string_lossy()));
    let ws = NSWorkspace::sharedWorkspace();
    let cfg = objc2_app_kit::NSWorkspaceOpenConfiguration::configuration();
    cfg.setCreatesNewApplicationInstance(true);
    let completion = RcBlock::new(
        |_app: *mut objc2_app_kit::NSRunningApplication, error: *mut NSError| {
            if error.is_null() {
                crate::app::delegate::dispatch_main_async(Box::new(|| {
                    let mtm = MainThreadMarker::new().expect("main thread");
                    NSApplication::sharedApplication(mtm).terminate(None);
                }));
            }
        },
    );
    ws.openApplicationAtURL_configuration_completionHandler(&url, &cfg, Some(&*completion));
}

/// Off the main thread: unzip, verify the chain, then `replaceItemAt`.
fn verify_and_install(zip_file: &Path) -> Result<(), InstallError> {
    let work = std::env::temp_dir().join(format!(
        "pastory-update-{}",
        objc2_foundation::NSUUID::new().UUIDString()
    ));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| InstallError::Other(e.to_string()))?;
    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _g = Guard(work.clone());
    run_tool(
        "/usr/bin/ditto",
        &[
            "-x",
            "-k",
            &zip_file.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    )
    .map_err(|_| InstallError::Archive)?;
    let new_app = work.join("Pastory.app");
    if !new_app.exists() {
        return Err(InstallError::Archive);
    }
    let target = bundle_path();
    let Some(mine) = team_identifier(&target) else {
        return Err(InstallError::Signature);
    };
    // Same team as the running copy, chained to Apple's Developer ID root, and
    // a valid signature — or nothing is touched.
    let verdict = format!("anchor apple generic and certificate leaf[subject.OU] = \"{mine}\"");
    run_tool(
        "/usr/bin/codesign",
        &[
            "--verify",
            "--deep",
            "--strict",
            &format!("-R={verdict}"),
            &new_app.to_string_lossy(),
        ],
    )
    .map_err(|_| InstallError::Signature)?;
    if team_identifier(&new_app).as_deref() != Some(mine.as_str()) {
        return Err(InstallError::Signature);
    }
    // replaceItemAt: swap only once everything checked out.
    let fm_obj: Retained<AnyObject> = unsafe {
        msg_send![objc2::class!(NSFileManager), defaultManager]
    };
    let target_url = NSURL::fileURLWithPath(&NSString::from_str(&target.to_string_lossy()));
    let new_url = NSURL::fileURLWithPath(&NSString::from_str(&new_app.to_string_lossy()));
    let mut resulting: *mut NSURL = std::ptr::null_mut();
    let mut error: *mut NSError = std::ptr::null_mut();
    let crate_name: Option<&NSString> = None;
    let ok: bool = unsafe {
        msg_send![
            &*fm_obj,
            replaceItemAtURL: &*target_url,
            withItemAtURL: &*new_url,
            backupItemName: crate_name,
            options: 0usize,
            resultingItemURL: &mut resulting,
            error: &mut error
        ]
    };
    // Keep swap target path alive for the rename to stay packaged.
    let _guard = Guard(target.clone());
    if ok {
        Ok(())
    } else {
        let msg = if error.is_null() {
            "replaceItemAt failed".to_string()
        } else {
            unsafe { &*error }.localizedDescription().to_string()
        };
        Err(InstallError::Other(msg))
    }
}

/// The relaunch target stays packaged (Guard above would drop it before the
/// swap finishes otherwise).
fn trash_placeholder_run_loop() {}

fn run_tool(tool: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(())
}

// Guard against the titled empty impl falling out of sync with Swift.
#[allow(dead_code)]
fn marker(_: &NSAlert) {}

/// NSObjectProtocol kept for the one place the ObjC surface needs it
/// (NSURLSession init patterns in update_progress.rs).
fn _unused(_: &dyn NSObjectProtocol) {}
