# Pastory — notes for coding agents

Menu-bar Mac app: clipboard history + screenshot annotation / OCR / region recording. **Rust（objc2 手写 AppKit，单 crate 二进制 pastory，链接系统 libsqlite3，无第三方 UI 框架）** —— 1.0.6 起仓库切到 Rust 管线（[docs/RUST_REWRITE.md](docs/RUST_REWRITE.md) §6 M7 替换决策，M0–M7 全 ✔）；`Sources/Pastory/` 里的 Swift 版冻结归档、仍是行为基准。Product copy: [README.md](README.md) (zh) / [README.en.md](README.en.md). Behaviour bible: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). Using the running app as memory for an assistant: [docs/AI.md](docs/AI.md).

## Hard rules

- Bundle id is `com.cici.snipclip`. **Do not change it.** Screen Recording, Accessibility, and all UserDefaults live under it.
- No third-party packages. One type per file, in `App/` `Capture/` `Annotate/` `Clipboard/` `Shelf/`.
- User data is `~/Library/Application Support/Pastory/`. Never point a self-test at it.
- Mutating `--selftest` commands require `PASTORY_STORE` set to a temp dir **outside** `~/Library/Application Support`. `PASTORY_STORE` / `PASTORY_LANG` are ignored on a normal launch (`App/Sandbox.swift`).
- Synthetic ⌘V is session-level (`CGEvent` → `.cgSessionEventTap`). Never HID — that latched Command machine-wide.
- `"导入"` (`ClipStore.importSourceName`) is a stored marker; do not translate it. UI shows 「已导入」.
- New user-facing string = Chinese literal + `.l` + a row in `App/Localization.swift`. `--selftest l10n` catches duplicate keys (they crash English launch).
- Open `build/Pastory.app` from Finder, not Terminal, or Screen Recording is attributed to Terminal.

## Build and self-test

```bash
./build-rs.sh                        # host arch → build/Pastory.app（./build.sh 是 shim，同入口）
BIN=./build/Pastory.app/Contents/MacOS/Pastory
S=/tmp/pastory-test
PASTORY_STORE=$S "$BIN" --selftest l10n
PASTORY_STORE=$S "$BIN" --selftest shelf /tmp/shelf.png
PASTORY_LANG=en PASTORY_STORE=$S "$BIN" --selftest shelf /tmp/shelf-en.png
```

Full command list is in `docs/DEVELOPMENT.md`. UI changes: render self-test in both languages. Restart the app with a separate command, never on the same line as a sandboxed self-test.

## Defaults (1.0.4)

⌥⌘S capture, ⇧⌘V shelf, ⌥⌘F search. Retention **0 = keep everything** until the user picks a schedule. Image storage HEIC. Password managers **not** recorded. Paste-on-double-click on. Capture permission on first screenshot, not at launch.
