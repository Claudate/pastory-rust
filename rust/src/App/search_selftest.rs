//! Port of `App/SearchSelfTest.swift` (`--selftest search`, M2): full-text
//! match against a reference computed with the same NSString calls, cache
//! invalidation, cancellation of obsolete work, `payloadReadCount`, and the
//! shelf model's selection state. Runs against a scratch store under the
//! system temp dir (the PASTORY_STORE gate lives in selftest.rs).

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use objc2_foundation::{NSNotFound, NSString, NSStringCompareOptions, NSUUID};

use crate::clipboard::item::{now, ClipItem, ClipKind};
use crate::clipboard::search_index::{Cancelled, ClipSearchIndex};
use crate::clipboard::store::{write_atomic, ClipStore, Source};
use crate::shelf::model::{ShelfFilter, ShelfModel};

fn check(ok: &mut bool, label: String, cond: bool) {
    println!("{} {}", if cond { "ok  " } else { "FAIL" }, label);
    *ok = cond && *ok;
}

/// The reference semantics from the Swift self-test: a literal search over
/// the lower-cased, precomposed full text.
fn reference_matches(store: &ClipStore, query: &str) -> HashSet<String> {
    let normalized = ClipSearchIndex::normalized_query(query);
    let needle = NSString::from_str(&normalized);
    store
        .items
        .iter()
        .filter(|item| {
            let joined = [
                store.text(item).unwrap_or_else(|| item.snippet.clone()),
                item.ocr_text.clone().unwrap_or_default(),
                item.source_app_name.clone().unwrap_or_default(),
                item.title.clone().unwrap_or_default(),
            ]
            .join("\n");
            let text = NSString::from_str(&joined)
                .lowercaseString()
                .precomposedStringWithCanonicalMapping();
            text.rangeOfString_options(&needle, NSStringCompareOptions::LiteralSearch)
                .location
                != NSNotFound as usize
        })
        .map(|i| i.id.clone())
        .collect()
}

fn one(id: &str) -> HashSet<String> {
    HashSet::from([id.to_string()])
}

fn ids(items: &[ClipItem]) -> Vec<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

pub fn run() -> bool {
    let root = std::env::temp_dir().join(format!(
        "pastory-search-{}",
        NSUUID::new().UUIDString()
    ));
    let ok = run_inner(&root);
    let _ = std::fs::remove_dir_all(&root);
    ok
}

fn run_inner(root: &Path) -> bool {
    let mut ok = true;
    let never = AtomicBool::new(false);
    let mut store = ClipStore::new(root.to_path_buf());
    let source = Source {
        bundle_id: None,
        name: Some("Search Fixture".into()),
    };
    let body = "prefix ".repeat(100) + "TailNeedle 中文 Cafe\u{301} 👍🏽 👨‍👩‍👧\r\nlast";
    let Some(first) = store.insert_text(&body, None, &source) else {
        return false;
    };
    let Some(second) = store.insert_text("second result", None, &source) else {
        return false;
    };
    store.set_title(Some("Fixture Title"), &second.id);
    let directory = root.join("items");
    let index = ClipSearchIndex::new();

    // MARK: ClipSearchIndex — match / cache / invalidation / cancellation

    let mut emoji_hits = 0;
    for query in [
        "tailneedle",
        "中文",
        "CAFÉ",
        "e\u{301}",
        "👍",
        "👩",
        "\n",
        "last",
        "fixture title",
        "search fixture",
        "missing",
    ] {
        let expected = reference_matches(&store, query);
        let actual = index
            .search(query, &store.items, &directory, &never)
            .expect("not cancelled");
        check(
            &mut ok,
            format!("full-text match for {query:?}"),
            actual == expected,
        );
        if (query == "👍" || query == "👩") && actual.contains(&first.id) {
            emoji_hits += 1;
        }
    }
    check(
        &mut ok,
        "a base emoji finds its skin-tone and family variants".into(),
        emoji_hits == 2,
    );
    let reads = index.payload_read_count();
    check(
        &mut ok,
        "repeated queries load each payload only once".into(),
        reads == 2,
    );
    // Pin / timestamps are not part of the document fingerprint.
    let mut changed = store.items.clone();
    for it in &mut changed {
        it.pinned = !it.pinned;
        let t = now();
        it.created_at = t;
        it.modified_at = t;
    }
    let _ = index
        .search("tailneedle", &changed, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "pinning and copying reuse the full-text cache".into(),
        index.payload_read_count() == reads,
    );
    store.update_text(&first.id, "replacement content");
    let old_matches = index
        .search("tailneedle", &store.items, &directory, &never)
        .expect("not cancelled");
    let new_matches = index
        .search("replacement", &store.items, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "editing invalidates the old payload".into(),
        old_matches.is_empty() && new_matches == one(&first.id),
    );
    store.set_title(Some("Renamed"), &second.id);
    let renamed = index
        .search("renamed", &store.items, &directory, &never)
        .expect("not cancelled");
    let old_title = index
        .search("fixture title", &store.items, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "renaming updates searchable metadata".into(),
        renamed == one(&second.id) && old_title.is_empty(),
    );
    let image = ClipItem::new(
        "ocr-fixture".into(),
        ClipKind::Image,
        now(),
        None,
        None,
        "640×480".into(),
        Some("recognized words".into()),
        false,
        "png".into(),
        false,
        Some(640),
        Some(480),
        0,
        None,
        None,
        1,
        None,
    );
    let before_ocr = index.payload_read_count();
    let mut with_image = store.items.clone();
    with_image.push(image.clone());
    let ocr_matches = index
        .search("recognized", &with_image, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "OCR search never reads image payloads".into(),
        ocr_matches == one(&image.id) && index.payload_read_count() == before_ocr,
    );
    let mut changed_ocr = image.clone();
    changed_ocr.ocr_text = Some("updated OCR".into());
    let mut with_changed_ocr = store.items.clone();
    with_changed_ocr.push(changed_ocr);
    let updated_ocr = index
        .search("updated ocr", &with_changed_ocr, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "completed OCR invalidates cached metadata".into(),
        updated_ocr == one(&image.id),
    );
    // `Task.cancel` before the task body runs: the first
    // `Task.checkCancellation()` throws. Arming the flag before the thread
    // starts keeps this deterministic (an async cancel racing a finished
    // scan is unobservable in the Swift original too).
    let cancel = AtomicBool::new(true);
    let cancelled_items = store.items.clone();
    let outcome = std::thread::scope(|scope| {
        scope
            .spawn(|| index.search("replacement", &cancelled_items, &directory, &cancel))
            .join()
            .expect("search thread")
    });
    check(
        &mut ok,
        "cancellation stops obsolete work".into(),
        matches!(outcome, Err(Cancelled)),
    );

    // MARK: ShelfModel — search state / selection / deletion

    let mut model = ShelfModel::owned(store);
    model.reset();
    model.update_search_blocking();
    model.set_query("replacement");
    // The previous list stays visible; acting on it is what is blocked.
    check(
        &mut ok,
        "pending queries cannot paste a stale selected item".into(),
        model.is_searching() && model.selected_item().is_none(),
    );
    model.update_search_blocking();
    check(
        &mut ok,
        "results select the first matching item".into(),
        !model.is_searching() && model.selected_item().map(|i| i.id) == Some(first.id.clone()),
    );
    let before_focus = model.items();
    model.focus_search += 1;
    check(
        &mut ok,
        "focusing search keeps completed results".into(),
        !model.is_searching() && model.items() == before_focus,
    );
    model.set_query("missing");
    let job = model.begin_search();
    std::thread::sleep(std::time::Duration::from_millis(2)); // `await Task.yield()`
    model.set_query("second");
    job.cancel();
    model.update_search_blocking();
    job.join();
    model.drain_completions();
    check(
        &mut ok,
        "rapid typing publishes only the latest results".into(),
        ids(&model.items()) == vec![second.id.clone()],
    );
    model.set_filter(ShelfFilter::Images);
    check(
        &mut ok,
        "category filters reuse matches and clear invalid selection".into(),
        model.items().is_empty() && model.selected_item().is_none() && !model.is_searching(),
    );
    model.set_filter(ShelfFilter::Text);
    check(
        &mut ok,
        "returning to text restores the matching selection".into(),
        model.selected_item().map(|i| i.id) == Some(second.id.clone()),
    );
    model.with_store_mut(|s| s.remove(&second.id));
    check(
        &mut ok,
        "deleted items disappear before the next search finishes".into(),
        model.items().is_empty() && model.selected_item().is_none(),
    );
    model.update_search_blocking();
    check(
        &mut ok,
        "deleted items stay absent from search".into(),
        model.items().is_empty() && !model.is_searching(),
    );
    model.set_query("   ");
    check(
        &mut ok,
        "clearing search shows history immediately".into(),
        !model.is_searching() && ids(&model.items()) == vec![first.id.clone()],
    );
    model.set_filter(ShelfFilter::All);
    let counts = model.counts();
    check(
        &mut ok,
        "filter counts update after deletion".into(),
        counts.get(ShelfFilter::All) == 1 && counts.get(ShelfFilter::Text) == 1,
    );

    // A temporarily missing payload falls back to its snippet, then is
    // retried (never cached as a permanent miss).
    let (path, items_snapshot) =
        model.with_store(|s| (s.payload_url(&s.items[0].clone()), s.items.clone()));
    std::fs::remove_file(&path).expect("payload removable");
    let recovering = ClipSearchIndex::new();
    let _ = recovering
        .search("replacement", &items_snapshot, &directory, &never)
        .expect("not cancelled");
    write_atomic(&path, b"restored content").expect("payload restored");
    let restored = recovering
        .search("restored", &items_snapshot, &directory, &never)
        .expect("not cancelled");
    check(
        &mut ok,
        "missing payloads are retried instead of caching a permanent miss".into(),
        restored == one(&first.id),
    );
    ok
}
