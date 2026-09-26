//! Port of `Clipboard/Retention.swift` — calendar-day retention, stateless
//! and idempotent.
//!
//! Rule: pick a cleanup hour X (default 04:00) and a retention of N days.
//! Whenever `sweep` runs:
//!   - if today's X has passed, "yesterday and earlier" (for N = 1) is
//!     expired; today's items are never touched;
//!   - if today's X has not come yet, only "the day before yesterday and
//!     earlier" is expired.
//! N = 3 shifts the line back two more days. Pinned items are never expired
//! by this code. Running it once or a hundred times gives the same result, so
//! it needs no "already cleaned" flag. N = 0 means never: nothing expires and
//! no timer is armed. It runs at launch and from one timer set to the moment
//! the oldest unpinned item expires (its day + N days, at X).

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_foundation::{NSCalendar, NSCalendarOptions, NSCalendarUnit, NSDate, NSRunLoop, NSTimer};

use crate::app::preferences::{Preferences};
use crate::clipboard::item::{ClipItem, start_of_day};
use crate::clipboard::store;

/// The armed one-shot timer, kept as a raw retained pointer: ObjC objects
/// inside the static would make the Mutex !Sync. The timer is only ever
/// armed from the main thread; `next_fire` reads it under the lock.
static TIMER: std::sync::Mutex<Option<usize>> = std::sync::Mutex::new(None);

/// One NSCalendar unit day.
const UNIT_DAY: NSCalendarUnit = NSCalendarUnit::Day;

fn calendar() -> Retained<NSCalendar> {
    NSCalendar::currentCalendar()
}

fn date_from(ts: f64) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSince1970(ts)
}

pub(crate) fn add_days(start: f64, days: i64) -> f64 {
    let cal = calendar();
    let d = date_from(start);
    cal.dateByAddingUnit_value_toDate_options(UNIT_DAY, days as objc2_foundation::NSInteger, &d, NSCalendarOptions(0))
        .map(|r| r.timeIntervalSince1970())
        .unwrap_or(start)
}

pub(crate) fn set_hour(base: f64, hour: i64, minute: i64, second: f64) -> f64 {
    let cal = calendar();
    let d = date_from(base);
    cal.dateBySettingHour_minute_second_ofDate_options(
        hour as objc2_foundation::NSInteger,
        minute as objc2_foundation::NSInteger,
        second as objc2_foundation::NSInteger,
        &d,
        NSCalendarOptions(0),
    )
    .map(|r| r.timeIntervalSince1970())
    .unwrap_or(base)
}

/// First calendar day that is still kept, for `now`.
pub fn keep_from_day(now: f64, cleanup_hour: i64, retention_days: i64) -> f64 {
    let today = start_of_day(now);
    let todays_cleanup = set_hour(today, cleanup_hour, 0, 0.0);
    // Before today's cleanup time, yesterday still counts as "current".
    let cutoff = if now >= todays_cleanup {
        today
    } else {
        add_days(today, -1)
    };
    add_days(cutoff, -(retention_days.max(1) - 1))
}

pub fn is_expired(item: &ClipItem, now: f64, cleanup_hour: i64, retention_days: i64) -> bool {
    if item.pinned {
        return false; // Pin = keep, always
    }
    if retention_days <= 0 {
        return false; // never clean up
    }
    let day = start_of_day(item.created_at);
    day < keep_from_day(now, cleanup_hour, retention_days)
}

/// Delete everything past the line; guard: never when the index cannot be written.
pub fn sweep(now: f64) {
    // Never delete files when the index cannot be written.
    if store::read(|s| s.last_save_failed) {
        return;
    }
    let p = Preferences::shared();
    let hour = p.cleanup_hour();
    let days = p.retention_days();
    let expired: Vec<String> = store::read(|s| {
        s.items
            .iter()
            .filter(|it| is_expired(it, now, hour, days))
            .map(|it| it.id.clone())
            .collect()
    });
    store::remove_where(&expired);
    store::purge_tombstones(30.0);
}

/// The instant an item created on `day` stops being kept: (day + N days) at hour X.
pub fn expiry_moment(item: &ClipItem, cleanup_hour: i64, retention_days: i64) -> Option<f64> {
    let day = start_of_day(item.created_at);
    Some(set_hour(add_days(day, retention_days.max(1)), cleanup_hour, 0, 5.0))
}

/// Launch: sweep now and arm the timer.
pub fn schedule() {
    sweep(now_ts());
    arm_timer();
}

/// After the cleanup hour or retention days change, or after a sweep.
pub fn reschedule() {
    arm_timer();
}

/// A new item arrived; if nothing was scheduled (store was empty), schedule for it.
pub fn item_added() {
    if TIMER.lock().unwrap().is_none() {
        arm_timer();
    }
}

/// `Date()` — seconds since 1970.
pub fn now_ts() -> f64 {
    NSDate::now().timeIntervalSince1970()
}

fn arm_timer() {
    let mtm = match MainThreadMarker::new() {
        Some(m) => m,
        None => return,
    };
    let p = Preferences::shared();
    let hour = p.cleanup_hour();
    let days = p.retention_days();
    // Compute the fire date BEFORE locking TIMER: the store calls us from
    // inside its own mutex (write_op recovery, prepend, un-pin) and
    // std::sync::Mutex is not reentrant, so the store read must not wait on
    // anything the store is holding. try_lock covers that path: re-arm right
    // after the current store call unwinds.
    let store_mutex = store::shared();
    let guard = match store_mutex.try_lock() {
        Ok(g) => g,
        Err(_) => {
            crate::app::delegate::dispatch_main_after(0.0, Box::new(arm_timer));
            return;
        }
    };
    let fire: Option<f64> = (|| {
        if days <= 0 {
            return None; // never: no timer at all
        }
        let store = guard.as_ref()?;
        if store.last_save_failed {
            return None; // re-armed by the store once a save succeeds
        }
        // Earliest expiry among unpinned items; nothing unpinned → nothing to schedule.
        let earliest = store
            .items
            .iter()
            .filter(|it| !it.pinned)
            .filter_map(|it| expiry_moment(it, hour, days))
            .reduce(f64::min)?;
        Some(earliest.max(now_ts() + 5.0))
    })();
    drop(guard);
    let mut timer = TIMER.lock().unwrap();
    if let Some(p) = timer.take() {
        // SAFETY: stored from a Retained below; invalidated + released here.
        let t: Retained<NSTimer> =
            unsafe { Retained::from_raw(p as *mut NSTimer) }.expect("stored timer is live");
        t.invalidate();
    }
    let Some(fire) = fire else { return };
    // A timer that was due during sleep fires as soon as the Mac wakes, so
    // sleep needs no special case.
    let fire_date = date_from(fire);
    // SAFETY: the block runs on the main run loop; `sweep` re-arms.
    unsafe {
        let block = block2::RcBlock::new(move |_t: std::ptr::NonNull<NSTimer>| {
            sweep(now_ts());
            arm_timer(); // next-oldest item, whenever that is
        });
        let block: &block2::DynBlock<dyn Fn(std::ptr::NonNull<NSTimer>)> = &block;
        let t = NSTimer::initWithFireDate_interval_repeats_block(
            mtm.alloc::<NSTimer>(),
            &fire_date,
            0.0,
            false,
            block,
        );
        t.setTolerance(60.0);
        let mode: &'static objc2_foundation::NSRunLoopMode = objc2_foundation::NSDefaultRunLoopMode;
        NSRunLoop::mainRunLoop().addTimer_forMode(&t, mode);
        *timer = Some(Retained::into_raw(t) as usize);
    }
}

/// Self-test / diagnostics: when the current timer will fire.
pub fn next_fire() -> Option<f64> {
    let guard = TIMER.lock().unwrap();
    let p = (*guard)?;
    // SAFETY: the pointer was stored from a Retained and the timer is alive
    // until armed-over or invalidated (both under the same lock we hold).
    unsafe { (p as *const NSTimer).as_ref() }.map(|t: &NSTimer| t.fireDate().timeIntervalSince1970())
}
