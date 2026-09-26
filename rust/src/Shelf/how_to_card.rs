//! Port of `Shelf/HowToCard.swift` — 「Pastory 怎么用」: an ordinary pinned
//! text card with the short manual, seeded once for every user (new or
//! upgrading). It behaves like any other card: searchable, editable, and
//! gone for good once deleted.

use crate::app::localization::is_english;
use crate::app::preferences::Preferences;
use crate::clipboard::store::{self, Source};

pub const VERSION: i64 = 2;
pub const TITLE_ZH: &str = "Pastory 怎么用";
pub const TITLE_EN: &str = "How to use Pastory";

pub fn seed_if_needed() {
    let p = Preferences::shared();
    let text = if is_english() { ENGLISH } else { CHINESE };
    // Seed only when the card can actually land on disk.
    if store::read(|s| s.load_failed || s.last_save_failed) {
        return;
    }
    let mut version = p.how_to_version();
    if version == 0 {
        // Older builds used a plain flag; treat it as version 1.
        version = if p.did_seed_how_to() { 1 } else { 0 };
        p.set_how_to_version(version);
    }
    if version == 0 {
        let item = store::with(|s| s.insert_text(text, None, &Source::pastory()));
        if store::read(|s| s.last_save_failed) {
            return;
        }
        if let Some(item) = item {
            store::with(|s| {
                s.set_title(Some(if is_english() { TITLE_EN } else { TITLE_ZH }), &item.id);
                if !item.pinned {
                    s.toggle_pin(&item.id, false);
                }
            });
        }
    } else if version < VERSION {
        // The card is still around: refresh its copy in place.
        let old = store::read(|s| {
            s.items
                .iter()
                .find(|it| {
                    (it.title.as_deref() == Some(TITLE_ZH) || it.title.as_deref() == Some(TITLE_EN))
                        && it.source_app_name.as_deref() == Source::pastory().name.as_deref()
                })
                .map(|it| it.id.clone())
        });
        if let Some(id) = old {
            store::with(|s| s.update_text(&id, text));
        }
    }
    p.set_how_to_version(VERSION);
    p.set_did_seed_how_to(true);
}

pub const CHINESE: &str = r#"1. 单击卡片：复制到剪贴板。
2. 双击卡片：可直接粘贴进你刚才所在的应用输入框（需要「辅助功能」权限）。
3. 拖动卡片：可拖出面板，固定到桌面任意位置，成为便签；便签可以编辑、调整大小、调整便签层级（浮动在所有窗口上 & 仅固定在桌面），可以随时关闭。

4. Pin：Pin 住的内容永远不会被自动清理。
5. 剪贴板历史清理：未 Pin 的内容默认永久保留，如想定时清理，可以在 设置 › 清理 选保留天数和清理时刻。已 Pin 内容永久保留，不受定时清理设置的影响。

6. 标题：选中卡片后点「加个标题」，方便辨认和搜索。
7. 搜索：历史较多时，可在搜索栏进行关键词搜索。

8. 截图 / 录屏：按截图快捷键，框选区域或点选窗口，即可截图、添加图片标注、识别图片中的文字；顶部可切到录屏，支持 MP4 & GIF 格式。
"#;

pub const ENGLISH: &str = r#"1. Click a card: copy it to the clipboard.
2. Double-click a card: paste it straight into the field you were typing in (needs Accessibility).
3. Drag a card: pull it out of the shelf and drop it anywhere on the desktop as a note. A note can be edited, resized, kept above all windows or on the desktop only, and closed at any time.

4. Pin: pinned items are never cleaned up automatically.
5. History cleanup: unpinned items are kept forever by default. To clean on a schedule, pick the days and the hour in Settings › Cleanup. Pinned items stay regardless of that schedule.

6. Titles: select a card and click "add a title" to make it easy to spot and search.
7. Search: with a long history, type a keyword in the search field.

8. Screenshots and recording: press the screenshot shortcut, drag a region or click a window to capture, annotate and recognize text; switch to Record at the top for MP4 or GIF.
"#;
