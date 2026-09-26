# Pastory → Rust 重写计划

> 活文档（living spec）：每个里程碑与关键组件带状态徽标，推进过程中逐项点亮更新。
> 徽标约定：○ 未开始 · ◐ 进行中 · ✔ 已完成。
> 行为基准 = [docs/DEVELOPMENT.md](DEVELOPMENT.md)（行为圣经）；本文只写「Rust 版怎么做、做到什么程度算对」。

| 里程碑 | 状态 |
| --- | --- |
| M0 工程骨架 + 菜单栏壳 | ✔ |
| M1 剪贴板核心（存储/监听/保留期） | ✔ |
| M2 Shelf 面板 UI + 搜索 | ✔ |
| M3 截图 + 框选 | ✔ |
| M4 标注 + OCR | ✔ |
| M5 录屏 + GIF | ✔ |
| M6 设置/便笺/导入/更新器 | ✔ |
| M7 双语对齐 + 替换决策 | ✔ |

---

## 1. 背景与动机

### 1.1 现状盘点

- Swift 版：52 个文件、10,428 行（`Sources/Pastory/`），SwiftPM + AppKit/SwiftUI，除系统 libsqlite3 外零第三方依赖。
- 四层能力：`App/`（入口/热键/权限/偏好/更新器/双语/自测）、`Capture/`（取屏/框选/截图/录屏/GIF）、`Annotate/`（标注画布/渲染/OCR）、`Clipboard/`（监听/存储/保留期/导入）、`Shelf/`（面板/卡片/编辑窗/便笺）。
- 数据：`~/Library/Application Support/Pastory/` 下 `pastory.sqlite`（WAL）+ `items/` + `thumbs/`。

### 1.2 为什么重写

作者决定用 Rust 重写整个 app（自用工具、想换技术栈）。重写不改变产品行为：Swift 版是行为基准，Rust 版逐项对齐后替换。

### 1.3 重写原则

1. **逐里程碑可验收**：每个 M 都以现有 `--selftest` 子命令为验收标准，不靠感觉。
2. **并行期不并行写**：Swift 版与 Rust 版共用同一存储目录，但同一时间只跑一个写库的实例，避免 WAL 写冲突。
3. **红线优先**（见 §2）：任何里程碑不得为省事破坏红线。
4. 数据零迁移：Rust 版直接读写现有 `pastory.sqlite` 与 `items/` 文件。

---

## 2. 红线与不变约束（不可侵犯清单）

1. **Bundle id `com.cici.snipclip` 不变。** TCC 的屏幕录制/辅助功能授权、全部 UserDefaults、更新器 Team 校验都挂在它上面。
2. **存储布局不变**：`pastory.sqlite` schema 逐列兼容（见 §5.2）、`items/<id>.<ext>`、`thumbs/<id>.heic`、`share/` 硬链接。不写迁移代码。
3. **合成 ⌘V 只发 `.cgSessionEventTap`**，发送前等用户修饰键全部松开，发送期间屏蔽本地键盘事件。**绝不发 `.cghidEventTap`** —— 2026-09-17 HID 层参与系统物理键盘记账，Command 键整机卡死到重启。
4. **`"导入"` 是存库标记，永不翻译**（`ClipStore.importSourceName`，`ClipStore.swift:317`）；界面显示走「已导入」。
5. **屏幕录制权限在第一次截图时请求**（`Permissions.ensureScreenRecording`），不在启动时；系统弹窗每次启动最多一次（`askedSystemThisLaunch`），之后弹自家提示，第一按钮是「重新启动 Pastory」（授权只对新进程生效）。
6. **无第三方 UI 框架**：不用 Tauri/egui/slint；UI 全部经 objc2 手写 AppKit。依赖只有 objc2 生态、rusqlite、gif（见 §3）。
7. **双语文案 = 中文字面量 + `.l` + 对表加一行**；带上下文的键写 `tab|图片`。对表是 `[(String,String)]` 数组不是字典 —— 重复键在 `--selftest l10n` 报错，而不是让英文启动即崩。
8. **`PASTORY_STORE` / `PASTORY_LANG` 只在带 `--selftest` 的命令行生效**（`App/Sandbox.swift` 全文逻辑）；正常启动一律忽略环境变量。
9. **contentHash 永远是原 PNG 的哈希**（HEIC/PNG 存储格式无关）；去重 = 同类型同哈希把旧卡 bump 到最前，不建新卡。
10. **Pin 住的条目永远不会被自动清理**；唯一连 Pin 一起删的是手动「移除所有导入条目」。

---

## 3. 技术选型与 crate 核验

> 核验日期 2026-09-24，逐个经 crates.io API 确认；「需验证」= crate 存在但具体 API 绑定覆盖要在对应里程碑 spike 里确认。

### 3.1 objc2 生态现状

- 主 crate `objc2` 最新稳定 **0.6.4**（2026-02-26）。已核实。
- 全部框架 crate 统一 **0.3.2**（2025-10-04，维护者 madsmtm，MSRV 1.71，双许可 MIT/Apache-2.0/Zlib）：
  `objc2-app-kit`、`objc2-screen-capture-kit`、`objc2-vision`、`objc2-carbon`、`objc2-core-graphics`、`objc2-core-foundation`、`objc2-core-text`、`objc2-image-io`、`objc2-av-foundation`、`objc2-core-video`、`objc2-core-image`、`objc2-metal`、`objc2-cocoa`（meta 箱）。均已核实。
- **`objc2-text-kit` 不存在（404）**：NSTextView / NSLayoutManager / NSTextContainer / NSTextStorage 的绑定都在 `objc2-app-kit` 里。已核实。

### 3.2 逐子系统选型表

| 子系统 | Swift 现状 | Rust 方案 | 状态 |
| --- | --- | --- | --- |
| AppKit UI | NSView/NSPanel/SwiftUI | objc2-app-kit 0.3.2 手写 AppKit（放弃 SwiftUI，全部 NSView + 手动布局） | 已核实 |
| 全局热键 | Carbon RegisterEventHotKey（`HotKeyCenter.swift`） | objc2-carbon 0.3.2（HIToolbox 绑定）；备选 global-hotkey 0.8.0（Tauri 维护） | 已核实 0.3.2 / 0.8.0 |
| 截图 | SCScreenshotManager.captureImage + SCContentFilter | objc2-screen-capture-kit 0.3.2 | 已核实；SCScreenshotManager 覆盖 需验证 |
| 框选浮层 | 每屏 borderless nonactivatingPanel + CGShieldingWindowLevel | objc2-app-kit NSPanel + objc2-core-graphics（CGShieldingWindowLevel 是纯 C 函数） | 已核实 |
| 标注渲染 | CGContext + NSGraphicsContext + TextKit | objc2-core-graphics + objc2-app-kit 的 NSTextStorage/NSLayoutManager | 已核实 |
| OCR | VNRecognizeTextRequest `.accurate` | objc2-vision 0.3.2 | 已核实 |
| HEIC/PNG | ImageIO CGImageDestination（保留内嵌色彩描述文件） | objc2-image-io 0.3.2 —— 继续走系统编解码保证与现有库字节兼容；不引纯 Rust HEIF | 已核实 |
| 录屏 MP4 | SCStream → AVAssetWriter（H.264/HEVC） | objc2-screen-capture-kit（SCStream）+ objc2-av-foundation + objc2-core-video（CVPixelBuffer） | 已核实 0.3.2；AVAssetWriter 覆盖 需验证 |
| GIF 编码 | 自写 AVAssetReader + ImageIO 管线 | `gif` crate 0.14.2（纯 Rust，2026-04-09），移植阶梯预算逻辑；解码侧 AVAssetReader 仍走 objc2-av-foundation | 已核实 0.14.2 |
| 剪贴板 | NSPasteboard 轮询 changeCount | objc2-app-kit NSPasteboard | 已核实 |
| SQLite | 系统 libsqlite3 C API | rusqlite **0.40.2**，`linked`（不开 bundled，链接系统同款引擎）；WAL / synchronous=NORMAL / secure_delete=ON / busy_timeout 全部 PRAGMA 可设 | 已核实 0.40.2 |
| 合成 ⌘V | CGEvent post(.cgSessionEventTap) | objc2-core-graphics 的 CGEvent / CGEventSource 绑定 | 已核实；post(tap:) 在 M0 spike 验证 |
| TCC/AX | CGPreflight/CGRequestScreenCaptureAccess、AXIsProcessTrustedWithOptions | objc2-core-graphics（CG 系列）；AX 在 ApplicationServices，用 objc2 的 extern 宏手 declare | 已核实；AX 需验证 |
| objc2 QuickLook | QLPreviewPanel（␣ 空格预览） | objc2-quartz 0.3.2（QuickLook 绑定在此）或手 declare 少量 selector | 已核实存在；QL 数据源协议 需验证 |
| 更新器 | URLSession + codesign 子进程 | std TcpStream/TLS（或极小的 http crate）+ `std::process::Command` 调 `/usr/bin/codesign` | 无新依赖 |

### 3.3 绑定不覆盖时的策略

objc2 框架 crate 全部支持 `declare` 特性：缺哪个 selector / C 函数，就地用 `extern_methods!` / `extern "C"` 补声明，**不为此引入第三方案**。预期需要手 declare 的点（在对应里程碑 spike 里逐一确认）：

- `SCScreenshotManager.captureImage`（async API 在 Rust 侧用 completion handler 形态）
- `AVAssetWriterInputPixelBufferAdaptor`
- `AXIsProcessTrustedWithOptions`（kAXTrustedCheckOptionPrompt）
- `CGShieldingWindowLevel`、`CGWindowLevelForKey(.desktopIconWindow)`

### 3.4 工具链

- Rust stable（edition 2021+），目标 `aarch64-apple-darwin` + `x86_64-apple-darwin`（`cargo build --target` + `lipo -create`，对应 build.sh 的 ARCHS 分支）。
- 构建/分发/发布脚本改造：`build-rs.sh`（或改写 build.sh 调 cargo）→ 同样的 `build/Pastory.app` 组装 + codesign 逻辑；dist.sh / release.sh 的 notarytool、spctl、ditto 流程不变，只换二进制来源。
- Info.plist / entitlements / 图标 / 字体资源全部沿用 `Resources/`。

### 3.5 本文档第一版遗漏、复查后补上的点（2026-09-24 复查）

逐文件重扫 Swift 源后补进规格的盲点，都在对应小节落地，这里汇总防止再漏：

1. **Quick Look**（`ShelfPanel.swift:237-246`）：面板 ␣ 走 `QLPreviewPanel.shared()` + 数据源协议；objc2 没有现成 QuickLook crate → 需要 `objc2-quartz` 0.3.2（已核实存在）或手 declare QLPreviewPanel 的极少数 selector。已并入 §5.13。
2. **CGWindowList 窗口排序**（`CaptureTarget.swift:43`）：pickableWindows 用 `CGWindowListCopyWindowInfo` 补 SCShareableContent 不保证的前后顺序。objc2-core-graphics 有绑定。已并入 §5.6。
3. **SyntheticMovie**（`App/SyntheticMovie.swift`）：`--selftest gif` 依赖它合成一段测试 MP4（AVAssetWriter 合成移块视频）——Rust 版自测也要能无录屏造出测试片源。已并入 M5 交付。
4. **缩略图缓存淘汰**（`ClipStore.swift:526-537`）：解码缓存 > 100 张时丢最老的 1/3（非全清）。已并入 §5.2。
5. **文本编辑窗的富文本**：`items/<id>.rtf` 旁文件 + NSTextView 属性串（`TextEditorWindow.swift:150`）；Rust 用 objc2-app-kit 的 NSTextView 同样处理。已并入 §5.14。
6. **Paste (wiheads) 导入的 LZFSE**：`Importer.swift:366` 解 `bvx…` 帧 —— 需要 `lzfse` crate 0.2.0（已核实存在，绑参考实现）或直接调系统 `Compression.framework`。已并入 §5.10。
7. **坐标换算**（`CaptureTarget.swift:52-66` 的 CoordinateSpace）：CG 全局（左上原点）↔ Cocoa 全局（左下原点）↔ display-local 三套坐标换算是框选/裁剪正确性的地基，Rust 版要单拎一个 module 并用 `capture` 自测的比对覆盖。
8. **多显示器的屏幕坐标**：`NSScreen.screens` 主屏高度参与换算（`CoordinateSpace.primaryHeight`），Rust 版 objc2-app-kit 的 NSScreen 等价读取。
9. **Quick Look 面板数据刷新**（`ShelfPanel.swift:246`）：QL 打开时数据变更要 `reloadData()` —— 并行期或编辑后窗口数据一致性。
10. **菜单栏图标 template 模式**（`Theme.swift:121`）：1x/2x 资源 + `isTemplate`，系统自动适配深浅色菜单栏 —— 不需要自己写暗色模式代码；纸感 UI 本身是固定配色不跟随系统暗色（与 Swift 版一致，无额外工作但要明确**不做**）。

---

## 4. 模块映射 / Cargo 结构

单 crate 二进制 `pastory`，模块目录沿用 Swift 的五个子目录（一类型一文件惯例保留：一个 Rust module 文件对应一个 Swift 文件）：

```
src/
  main.rs                 ← App/main.swift
  App/
    delegate.rs  hotkey.rs  permissions.rs  preferences.rs  localization.rs
    theme.rs  sandbox.rs  selftest.rs  updater.rs  update_progress.rs
    settings_window.rs  synthetic_movie.rs  annotation_text_selftest.rs  search_selftest.rs
  Capture/
    coordinator.rs  target.rs  screenshotter.rs  selection_overlay.rs
    screen_recorder.rs  recording_session.rs  recording_preview.rs
    gif_encoder.rs  pasteboard_writer.rs
  Annotate/
    annotation.rs  renderer.rs  text_layout.rs  text_view.rs
    annotate_view.rs  toolbar.rs  top_bar.rs  ocr.rs  ocr_panel.rs
  Clipboard/
    item.rs  store.rs  db.rs  monitor.rs  search_index.rs  retention.rs  importer.rs
  Shelf/
    panel.rs  view.rs  card.rs  settings_pane.rs  contact_pane.rs
    text_editor.rs  image_editor.rs  exporter.rs
    desktop_notes.rs  desktop_note_view.rs  welcome_card.rs  how_to_card.rs
```

Swift → Rust 一一对应清单（52 个文件全覆盖，上表即映射；`App/SelfTest.swift` 的各渲染自测按命令拆进 `App/selftest/*.rs`）。

**Cargo 单 crate + feature 无关**：`--selftest <cmd>` 是运行时参数（与 Swift 一致），不是编译期 feature。

---

## 5. 关键子系统设计

> 每节 = Rust 实现要点 + 精确锚定到 Swift 源的基准行为。验收一律回指 §7 的 selftest。

### 5.1 剪贴板轮询（基准 `Clipboard/ClipboardMonitor.swift`）

- Timer 每 **0.5 s** 读 `NSPasteboard.general.changeCount`（macOS 无变更通知，这是标准做法）。
- `skipTypes`：`org.nspasteboard.ConcealedType`、`org.nspasteboard.TransientType`、`org.nspasteboard.AutoGeneratedType`、`com.agilebits.onepassword` —— 上板带任一即不记录。
- 密码管理器：`isPasswordManager` = 12 个已知 bundle id 精确匹配 + 9 个子串（password/keepass/bitwarden/enpass/dashlane/lastpass/nordpass/strongbox/protonpass），`recordPasswordManagers` 关闭时生效。**前置应用追踪**：`previousFront (bundleID, until)` 记录刚失焦的应用，复制后 1.5 s 内切走的密码管理器复制仍按密码管理器归属（防 ⌘-Tab 误归属）。
- 锁屏：DistributedNotificationCenter 监听 `com.apple.screenIsLocked` / `com.apple.screenIsUnlocked`；锁屏期间不记录，解锁时 `lastCount = changeCount` 重置（忽略锁屏期间的变更）。
- 暂停开关：`Preferences.monitoringPaused`。
- **ingest 规则**（`ClipboardMonitor.swift:87-127`，顺序即行为）：
  1. 上板带自家 marker（`com.cici.snipclip.marker`）→ 读出 item id → `bump` 该卡，不建新卡；
  2. skipTypes 命中 → nil；
  3. 位图（png/tiff）+ 无 URL 或恰好 1 个图片文件 URL（截图工具 handoff）→ `insertImage`；
  4. 恰 1 个图片文件 URL 且在临时目录（`/T/`、`/tmp`、`/private/tmp`、`/Caches/`、`/Library/Containers/`）→ 解码为 PNG → `insertImage`；
  5. 其他 URL → `insertFiles`（只存引用）；
  6. 字符串 → `insertText`（附 rtf）；URL 字符串 → `insertText`。
- Rust 侧：NSTimer 换成主线程 dispatch timer；NSWorkspace.didActivateApplicationNotification 经 objc2 通知中心注册。

### 5.2 ClipStore + SQLite（基准 `Clipboard/ClipDB.swift` + `ClipStore.swift`）

**开库 PRAGMA**（顺序保留）：`busy_timeout=3000` → `journal_mode=WAL` → `synchronous=NORMAL` → `secure_delete=ON`（删行覆零）。

**items 表**（逐字兼容，不迁移）：

```sql
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, created_at REAL NOT NULL,
    source_bundle TEXT, source_name TEXT, snippet TEXT NOT NULL, ocr_text TEXT,
    pinned INTEGER NOT NULL DEFAULT 0, ext TEXT NOT NULL, has_rtf INTEGER NOT NULL DEFAULT 0,
    pixel_w INTEGER, pixel_h INTEGER, byte_count INTEGER NOT NULL DEFAULT 0,
    duration REAL, title TEXT, content_hash INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS items_created ON items(created_at DESC);
-- 迁移：modified_at 列缺失时 ALTER TABLE 加列并 UPDATE items SET modified_at = created_at
CREATE TABLE IF NOT EXISTS tombstones (id TEXT PRIMARY KEY, content_hash INTEGER NOT NULL, deleted_at REAL NOT NULL);
```

- `kind` ∈ `text|url|image|files|video`。
- 单条改动 = 单行 upsert（17 列）/ delete；只有**清空**和**导入**走整表重写，包在 `BEGIN IMMEDIATE`。
- 索引读不出 → 不写任何东西；索引写失败 → UI 回滚、清理暂停、提示一次，直到写回成功（`lastSaveFailed`）。退出前 `PRAGMA wal_checkpoint(TRUNCATE)`。
- **contentHash**：SHA256(payload) 取前 8 字节按小端成 `i64`（`stableHash`，`ClipStore.swift:6`）。图片项的哈希**永远是原 PNG 的**，与存储格式无关；编辑改图后重算。
- **去重**：同类型 + 同哈希 → `bump(id)` 提到最前（文本+链接一族、图片一族分别匹配）。marker 回环也走 bump。
- 单条文本 > **20 MB**（`maxTextBytes = 20*1024*1024`）不入库。
- id：时间戳格式 `transient=-1` 保留（导入占位用）。
- 文件布局：`items/<id>.txt`（+`.rtf`）、`.heic|.png`、`.json`（文件路径列表）、`.mp4|.gif`；`thumbs/<id>.heic` 长边 900px q0.8（老库 `.png` 兜底）。
- **缩略图缓存**：解码缓存 > 100 张时丢最老的 1/3（不是全清，清掉屏幕上的会闪）；`version` 计数器每次成功写入 +1，UI 的派生列表按它失效。
- OCR：图片入库后后台线程跑（Swift 是 Task.detached），完成后 upsert 该行。
- `png(of:)`：HEIC 项解码再包 PNG 交给剪贴板/编辑器/导出 —— 粘贴和导出永远无损。

### 5.3 保留期（基准 `Clipboard/Retention.swift`）

自然日算法，无状态、幂等：

- `keepFromDay(now, X, N)`：`cutoff = now >= 今天的X ? 今天 : 昨天`；返回 `cutoff - (max(1,N)-1) 天`。即：过了今天的清理时刻 X → 「昨天及更早」到期（N=1）；没到 X → 「前天及更早」到期。
- `isExpired`：pinned 永不、`N==0` 永不；否则 `startOfDay(createdAt) < keepFromDay`。
- `expiryMoment`：`(startOfDay(createdAt) + N 天) 的 X:00:05`。
- 触发点只有两个：**启动时一次** + **一个定在最早到期时刻的一次性 Timer**（tolerance 60s；睡眠错过的醒后补跑）。`N=0`：永不 + 不振 timer。清理后重新取最早已到期时刻再振。
- `lastSaveFailed` 时挂起清理（索引写不回就不删文件）。
- 清理同时 `purgeTombstones()`（> 30 天的墓碑清掉）。

### 5.4 搜索索引（基准 `Clipboard/ClipSearchIndex.swift` + `Shelf/ShelfPanel.swift:393`）

- actor（Rust：`tokio` 不引，直接 std thread + mutex 即可，规模小）。
- 归一化：lowercase + Unicode NFKC precomposed（`normalizedQuery`）。
- 匹配：对归一化后文本做**字面量**（literal）子串匹配 —— 不按字素簇严格比较，👍 能搜到 👍🏽。
- 正文只读前 **200,000 字符**；拼 `正文 + ocr + source + title` 参与匹配。
- 文档指纹（kind/hash/ext/snippet/ocr/source/title）相等才复用缓存，否则重读 payload；空查询后台预热全文。
- UI 侧：80 ms 防抖（`Task.sleep(80_000_000ns)`）、新查询取消旧查询、旧结果列表在上屏新结果前保持可见（不闪空）、动作只允许作用于当前请求对应的结果（防贴错旧结果）。
- `payloadReadCount` 自测指标移植（`--selftest search` 断言不重复读）。

### 5.5 合成 ⌘V 与权限（基准 `App/Permissions.swift`）

- `sendPaste(retries:12)`：`CGEventSource.flagsState(.combinedSessionState)` 与 `⌘⇧⌥⌃` 有交集 → 50 ms 后重试（12 次）直到用户修饰键抬完。
- 发送：`CGEventSource(stateID: .combinedSessionState)`，keyCode **9**（kVK_ANSI_V），down+up 两事件 `flags = .maskCommand`，`post(tap: .cgSessionEventTap)` —— **红线：绝不 `.cghidEventTap`**。
- 发送期间 `setLocalEventsFilterDuringSuppressionState([.permitLocalMouseEvents, .permitSystemDefinedEvents], .eventSuppressionStateSuppressionInterval)` 屏蔽本地键盘。
- 粘贴到来源应用（`ShelfPanel.swift:144-157`）：双击/⏎ → 面板收起 → 等 0.25 s → 确认 `frontmostApplication == previousApp`（每 0.06 s 一次，最多 8 次）→ `sendPaste()`。无辅助功能权限时退化为只复制并收起（并弹请求）。
- `previousApp` 记录：面板显示时记住 `frontmostApplication != self` 的那个。
- 屏幕录制：`CGPreflightScreenCaptureAccess()` 探测；首次截图时 `CGRequestScreenCaptureAccess()`，每启动最多一次系统弹窗；被拒弹自家三按钮提示（重启 / 打开设置 / 取消）。
- 辅助功能：`AXIsProcessTrustedWithOptions(kAXTrustedCheckOptionPrompt: true)`，同样每启动最多一次。

### 5.6 截图（基准 `Capture/Screenshotter.swift` + `Capture/CaptureTarget.swift`）

- `SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)` 取内容；`ShareableSnapshot` 记下自家窗口（pid 匹配）以便排除/特殊点名。
- 目标三种：整屏（display filter excluding own windows）、区域（display + CGRect，display-local 点，原点左上）、单窗（desktopIndependentWindow filter）。
- **窗口排序**：SCShareableContent 不保证顺序，`pickableWindows` 用 `CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements])` 的列表序补前后关系（用户看到的最上层窗口排第一）。
- `SCStreamConfiguration`：`captureResolution = .best`、BGRA、无光标、显示 ownWindows 排除。
- **像素比** `pixelScale`：优先 `NSScreen.backingScaleFactor`，否则按 filter 实际像素/点算。
- 色彩：截图保留显示器自身色彩空间（Display P3 等），ImageIO 编码时内嵌 profile —— 不做 sRGB 拍平。
- 存储：默认 HEIC q0.9（`Preferences.imageStorage` 可改 png 无损）；`heicData`/`pngData` 走 ImageIO；TIFF→PNG 用 NSBitmapImageRep 等价（保内嵌 profile）。
- 缩略图：`thumbnail(maxPixels:900)` 长边 900、q0.8 HEIC（失败落 PNG）。
- Rust：objc2-screen-capture-kit；async capture 用 completion-handler 版本 + 自旋/信号量封装。

### 5.7 框选浮层（基准 `Capture/SelectionOverlay.swift`）

- 每个显示器一个 `NSPanel`：`borderless + nonactivatingPanel`、`level = CGShieldingWindowLevel()`（盖过其他工具的浮动条）、`collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]`、`canBecomeKey = true`。
- 内容：冻结底图 + 0.5 透明暗罩 + 选区挖孔（只对暗罩挖孔，冻结画面原样透出）。
- 按键瞬间**先冻结鼠标所在屏**，再激活自己、让面板成为 key window（前台应用收起的菜单已在照片里；系统不允许后台进程改光标，不激活就没有十字光标）。
- 交互：拖橡皮筋出矩形；选完后框 + 8 手柄可调；`⎋` 取消、`␣` 切整屏/窗口/区域模式、`F` 随窗口、`⏎` 确认、再按截图热键 = 重新框选。无修饰键热键用 `HotKeyCenter.bindRaw`（Carbon，keyCode 49 = Space 等），标注器出现后解绑。
- 结束把焦点还给原应用；取选区换算 `region → displayLocalRect` 再 `crop`。
- 录制就绪态：框和手柄照常可调、标注工具隐藏、底部一条「拖动边框调整录制范围 · 取消 · 开始录制 ⏎」；顶部 截屏/录屏 随时互切；只有「开始录制」/⏎/双击才开录。

### 5.8 标注编辑器（基准 `Annotate/Annotation.swift` + `AnnotationRenderer.swift` + `AnnotationTextLayout.swift`）

- 7 工具 + 单键：rect `r`、ellipse `o`、arrow `a`、line `l`、pen `p`、text `t`、mosaic `m`；线宽 S/M/L（1/2/3）。
- 调色盘 7 色：紫 `#B5A3F2` + `#E9631A` `#C56F8C` `#A9C2E0` `#59382C` `#1E151C` `#EBEBDF`。
- **手绘感渲染**：路径打平、长段加密后加低频噪声，**画两遍**；随机源 SplitMix64 固定 seed（`Seeded(20260912)` 同族）→ 预览与导出逐像素一致。`--selftest annotate` 输出 `<out>.flat.png` 合成结果比对。
- 马赛克：像素块化（源图分块取均值）。
- 文字框：TextKit（NSTextStorage + NSLayoutManager + NSTextContainer）布局，**编辑与导出共用同一排版**（`AnnotationTextLayout`）；新框宽度跟随文字、碰到图片右缘才换行；拖过角/边中点后宽度固定、保留字号自动换行、高度至少容纳完整内容；编辑重开保留框尺寸（`committedBoxSize`）。
- 交互：⏎ 换行；⌘⏎ 或点框外确认（⎋ 放弃）；点框外只确认不新建，再点才开始下一个；确认后 ⏎ = 完成整张截图。空画布拖 = 整区移动。`⌘Z` 单级 undo（popLast）。`⌫` 删选中。recordMode 下隐藏标注、`⌘Z`/删除失效。
- 渲染出口：纯 CGContext 离屏（Rust：objc2-core-graphics 直接画），翻转坐标与 NSGraphicsContext 对齐。
- ⌥⌘S 入口（`Capture/CaptureCoordinator.swift`）：start → pick → annotate → 完成 = 复制 + 入库（图 + OCR 文本）；⌘⏎ 完成。

### 5.9 录屏 + GIF（基准 `Capture/ScreenRecorder.swift` + `GIFEncoder.swift`）

- SCStream：30 fps（`minimumFrameInterval = 1/30`）、BGRA、`queueDepth = 6`。
- 自管 AVAssetWriter（mp4）：`AVVideoCodecType.hevc/h264` 按设置 `recordHEVC`；平均码率 = `宽*高*fps*bpp`，**bpp = H.264 0.1 / HEVC 0.065**，夹在 3–30 Mbps；停止时把最后一帧补一次再 finish。
- 上限 **10 分钟**自动停。启动窗口期点「丢弃」也要停掉采集流。
- GIF（`gif` crate 重写编码侧，预算/降级逻辑照抄）：
  - 预算按时长阶梯：≤10 s **6 MB**、≤30 s **10 MB**、≤60 s **15 MB**、更长 **20 MB**；
  - 起点：长边 ≤ **1280**、**10 fps**；超预算先 `fps→8`，再按 `sqrt(budget/estimate)` 缩尺寸，再超 `fps→6`；
  - 编码一次，超出预算 1/4 以上 → 缩小重编。
- 预览窗（`RecordingPreviewWindow.swift`）：录完选择 MP4 / GIF / 丢弃；`--selftest preview` 渲染。
- **SyntheticMovie**（`App/SyntheticMovie.swift` 的 Rust 等价物）：`--selftest gif` 需要一段合成测试 MP4（AVAssetWriter 合成移块 H.264 视频，秒数/尺寸/帧率参数化）—— Rust 版必须能无录屏自造片源，否则 gif 自测依赖真机录制。

### 5.10 导入（基准 `Clipboard/Importer.swift`）

- **永远在临时副本上读**（先 copy 到临时目录再开 SQLite）。
- 三种读法：Pastory 库按 schema 精确导；Paste（wiheads，Core Data：`ZITEMENTITY → ZITEMDATAENTITY.ZRAWPASTEBOARDITEMS`，archive 内嵌或外部文件，LZFSE 帧 `bvx…` 解包，首字节 0x01 —— Rust 侧 `lzfse` crate 0.2.0（已核实）或系统 Compression.framework）；其他 SQLite 按启发式（表名/列名猜：日期列、pin 列、blob 列）。
- 目录扫描：按文件头识别 SQLite，最多 **3 层**深，超过 **6 个库**或目录过宽（家目录、Library 等）直接拒绝。
- 防 trip：`id` / `ext` 含 `/` 或 `..` 的行跳过。
- 导入一批**整体排在自己记录之后**（导入时间序号在现有 max 之后）；来源 `source_name = "导入"`（红线 4）。
- 已删内容不复活：`seen ∪ tombstones` 的哈希跳过。
- 保留期非「永不删除」时，导入确认框只给「导入并改为永不删除」一个入口。

### 5.11 更新器（基准 `App/Updater.swift` + `App/UpdateProgressWindow.swift`）

- 启动 30 s 后、之后每 24 h，请求 `api.github.com/repos/nothingbutcici/pastory/releases/latest`（**全 app 唯一网络请求**，设置可关 `checkForUpdates`）。tag 必须 `v<版本>`，资产 `.zip`。
- 版本比较 `isNewer` 按数字段逐段比；`skippedVersion` 记住跳过的版本。
- 自动安装**仅限 Developer ID 签名的运行包**（`canSelfInstall`：TeamIdentifier 非空 + 未被 App Translocation 挪走）：下载（进度窗、可取消、30 s 无响应判失败）→ 后台解压 → `codesign --verify --deep --strict -R="anchor apple generic and certificate leaf[subject.OU] = <本 Team>"` → Team 与运行中自己一致 → `replaceItemAt` 原地替换 → 重启。
- ad-hoc 包 / Translocation：只打开下载页。
- Release notes 解析：跳过 `Pastory <版本>` 标题行与 `---`。
- dist.sh / release.sh 流程不变（notarytool profile `pastory-notary`、spctl 应显示 `source=Notarized Developer ID`、首次打开.txt）。

### 5.12 桌面便笺（基准 `Shelf/DesktopNotes.swift` + `DesktopNoteView.swift`）

- 面板里的卡**向上拖出面板边界**即成便笺（松手仍在面板上 = 取消）；右键「贴到桌面」。
- 同一张纸卡、全文展示（文本最高屏高 60%，超出卡内滚动）；底部 编辑 / 图层 / 关闭；单击复制、双击粘贴；非激活面板不抢焦点。
- 贴上即自动 Pin；面板里的卡保留并带 note 标记；卡被删便笺同删（`itemsGone`）。
- 层级逐张决定：浮于窗口上 = `NSWindow.Level.floating`；仅桌面 = `CGWindowLevelForKey(.desktopIconWindow) + 1`。所有 Space 可见。
- 位置存偏好 `desktopNotes`（`[{id,x,y,w,h,top}]` 屏幕坐标），启动恢复并夹回可见屏幕；store 读不出时不清理位置（`loadFailed` guard）。
- 发现路径：欢迎卡「试一试」、卡片右键、首次启动种进历史的 Pin 卡「Pastory 怎么用」（`HowToCard.swift`，普通卡片，删了不再出现；`didSeedHowTo` / `howToVersion`）。

### 5.13 Shelf 面板与卡片（基准 `Shelf/ShelfPanel.swift` + `ShelfView.swift` + `ClipCardView.swift`）

- 面板：borderless `nonactivatingPanel`、不透明背景关掉、level `.statusBar`；显示时按鼠标所在屏铺满宽、高 = `max(384, 屏幕 48% − 36)`；内容从 `-slide` 高度滑入（animator 0.18 s）；点击面板外自动收起；`applicationShouldHandleReopen` 也开面板。
- **␣ Quick Look**（`ShelfPanel.swift:237-246`）：`QLPreviewPanel.shared()` + 面板实现 QL 数据源；QL 可见时数据变更要 `reloadData()`。Rust 侧 `objc2-quartz` 0.3.2（已核实存在）或手 declare。
- 布局：左侧棕色侧栏（Pastory 手写体 + 剪贴板/设置/联系我），右侧 tab 筛选（全部/文本/图片/录屏 + 计数）+ 搜索框 + 关闭；横向卡片流（间距 16、卡宽 288）；底部纸色细滚动条（可拖）。
- 纸感主题（`App/Theme.swift`，Rust 逐值复刻）：
  - 色板：紫 `#B5A3F2`、棕 `#2B211E`、深棕 `#241C19`、纸 `#F2EDE3`、纸暗 `#E6DFD1`、纸蓝 `#BDD6E5`、深纸蓝 `#7FA5BD`、墨 `#2A2521`、墨淡 `#6E665F`、棕上文字 `rgb(0.93,0.90,0.86)`、棕上淡字 `rgb(0.68,0.63,0.59)`。
  - 字体：正文宋体 `STSongti-SC-Regular/Bold`；手写体 Caveat + CJK 级联 `HanziPenSC-W5`（fontDescriptor cascadeList）；品牌字 Ysabeau Office 只用于「Pastory」。`Resources/Fonts/` 里打包 `YsabeauOffice.ttf` / `Caveat.ttf`，启动注册。
  - 纸粒噪声：SplitMix64 seed **20260912** 的平铺 grain；`paperTile` grain 0.11、`deskTile` grain 0.22 预烘焙成 tile（Rust：离屏 CGContext 生成 NSImage 等价物）。
  - 撕纸边 `TornPaper`：seed 固定逐形状不闪；`RuledBox` 三边手绘框（无顶边）。
- 卡片交互（`ShelfPanel.swift:199-215` 键盘表 + `:478-502` 复制流）：
  - 单击复制（键盘交还延迟到双击间隔之后）；双击 = 复制+收起+粘贴；⏎ 只在设置「双击+回车」且**亲手选中**（方向键选的，搜索/筛选/删除后自动落点不算）时粘贴；← → ↑ ↓ 选；␣ Quick Look；⌘P Pin；⌘S 保存；⌫ 删除（Pin 项确认）；⌘F/直接打字搜索；⎋ 依次关重命名框/返回/清搜索/收起。
  - `copyAndClose` 带 0.6 s 防重入（`lastCloseAt`）。
  - 右键菜单全项（复制/复制并关闭/命名/去标题/编辑/预览/Pin/贴到桌面/保存到本地/在 Finder 中显示/打开/复制识别出的文字/删除）。
- 状态恢复：面板隐藏时记住屏幕位置；`frameOnScreen`。

### 5.14 编辑窗 / 导出 / 设置（基准 `Shelf/TextEditorWindow.swift` + `ImageEditorWindow.swift` + `Exporter.swift` + `Shelf/SettingsPane.swift`）

- 文本编辑窗 620×460（NSWindow + NSTextView，纸感）；保存 = 重写条目内容 + 重算哈希 + 写剪贴板。图片编辑窗复用标注画布（ImageEditorWindow 实现 AnnotateDelegate），保存同理（`updatedContent` 后 bump 重排）。
- 导出：图片永远无损 PNG（HEIC 解码→PNG）；文件名 `Pastory <时间戳>.png`；默认 `~/Downloads`（`exportDir` 偏好可改，不存在则建）。
- 设置页（纸感）：快捷键录制（当场试注册：被占用/与自己重复拒绝；无修饰键单键拒绝；只有 ⌘/⇧+单键允许但提示会覆盖所有应用——录成后通知 `shortcutsChanged` 重绑）、清理时刻/天数、图片格式、HEVC、粘贴模式（off/double/return，读值时兼容旧 `pasteOnDoubleClick` 布尔）、语言（system/zh/en，切换后整个 shelf 重建 `langTick`）、更新开关、权限徽标 1 s 轮询。
- 设置页/联系我打开时面板键盘只认 ⎋ 和 ⌘F。

### 5.15 启动顺序（基准 `App/AppDelegate.swift:16-46`）

Rust main 对齐同一顺序：

1. `--selftest` 检测（`Sandbox.isSelfTest`：第一个 `--selftest` 参数且后面还有参数）→ 走自测分支即返回；
2. statusItem（菜单栏手写 P 图标 `MenuIcon.png`，template 模式）+ 左/右键菜单；
3. `bindShortcuts()`（监听 `shortcutsChanged` 重绑）；
4. `Retention.schedule()`（sweep + armTimer）；
5. `ClipboardMonitor.start()`；
6. `ShelfPanel.prewarm()`（预建面板 + 后台预热搜索）；
7. `DesktopNotes.restore()`；
8. `HowToCard.seedIfNeeded()`；
9. `Updater.schedule()`；
10. 首启 `!didWelcome` → 面板自动打开 + 欢迎卡（`didWelcome` 置位）；
11. 菜单随语言变化重建（`languageChanged`）。

---

## 6. 里程碑

### M0 ✔ 工程骨架 + 菜单栏壳

交付：cargo 工程（§4 目录）、Info.plist/entitlements/资源沿用、`build-rs.sh` 产出可签名的 `build/Pastory.app`（Developer ID 自动探测逻辑同 build.sh）、菜单栏图标 + 菜单 + 退出、Sandbox 环境开关、`.l` 框架（空表）。
验收：从 Finder 打开能看到菜单栏图标；`--selftest l10n` 通过；正常启动忽略 `PASTORY_STORE`。
spike：CGEvent post(tap:)、objc2-app-kit 最小 NSPanel、字体注册 三件套在本里程碑打穿。
完成记录（2026-09-25）：三 spike 全部真机打穿——`--selftest paste`（session-tap post，flags 在 2s 内回到 post 前值；本机 idle 态常见 Command 位常置，Swift 版同样如此，故判定用回 settle 而非绝对值）、`--selftest panel`（Borderless+NonactivatingPanel+CGShieldingWindowLevel）、`--selftest fonts`（CTFontManagerRegisterFontsForURL .process 注册 YsabeauOffice/Caveat）。`--selftest l10n` 通过（26 entries，无重复键）；正常启动忽略 `PASTORY_STORE`（自测 suite 域未创建）；菜单栏图标 + 菜单经 `open` 启动 + System Events 验证。cargo 产物经 build-rs.sh 打包签名（ad-hoc；Developer ID 自动探测逻辑同 build.sh）。已知机器态：本机 idle 时 combinedSessionState 常置 Command 位（0x20100000），Swift 版同样如此，paste 自测因此改为 delta 判定。

### M1 ✔ 剪贴板核心

交付：ClipItem/ClipStore/ClipDB（schema 逐字 §5.2）、ClipboardMonitor + ingest、Retention、PasteboardWriter（marker + png/tiff/string/rtf/URL/gif/NSFilenames）、HEIC/PNG/ImageIO、tombstones、导入入口 stub。
验收：`--selftest` 的 `ingest`、`retention`、`tombstone`、`heic`、`writer`、`clipboard 10`、`pbtypes`、`pbfiles` 全绿（mutating 命令一律 `PASTORY_STORE=/tmp/pastory-test`）。
完成记录（2026-09-25）：交付与验收全部落地——`ingest`（六条规则 5 项断言）、`retention`（公式表 + 幂等 + 重载后手动删除不复发 + N=0 不振 timer + 7 天 cadence 精确到秒）、`tombstone`（删除→导入旧副本 skips→新拷贝复活）、`heic`（HEIC 更小、尺寸/Display P3 色彩空间保真、png(of:) 无损回包、同 PNG 哈希去重）、`writer`（string/rtf/URL/png/tiff/gif/NSFilenames + marker 全上板）、`pbtypes`/`pbfiles`、`clipboard`（pbcopy 实时落库打印）；M0 的 l10n/fonts/paste 复跑仍绿，debug 与 release（build-rs.sh 产出，ad-hoc 签名）两档均过，`cargo build` 零警告。
工程要点：① stableHash 走 CommonCrypto `CC_SHA256`（libSystem 内建，未加 sha2 依赖），前 8 字节小端 i64 与 CryptoKit 一致（heic 去重断言背书）；② store 为进程级单例 Mutex，缩略图后台解码经锁提交；`arm_timer` 用 `try_lock` + `dispatch_main_after(0)` 兜底——store 写栈内触发 reschedule 时先 defer，标准 Mutex 不可重入，自测 reschedule 路径仍同步；③ 模块用 `#[path = "App/…"]` 显式挂载，沿用 Swift 大写目录但不依赖文件系统大小写，`crate::` 路径全小写；④ objc2 0.3.2 生成绑定的踩坑记录：CG/CF 一律 cf_type 关联函数形态（`CGImage::width(Some(&g))`）、返回值用 `objc2_core_foundation::CFRetained`；AppKit 保持 ObjC 大小写（`setData_forType`）；`NSDistributedNotificationCenter` 在 objc2-foundation 而非 app-kit；`NSPasteboard.releaseGlobally` 与 `NSWorkspaceApplicationUserInfoKey` 未绑定（分别用 msg_send 与字面量 NSString 补）；观察者令牌不存（进程级泄漏，与 Swift 一致）。⑤ Importer 扫描地基（临时副本+WAL 旁车、目录 3 层 6 库上限、过宽目录拒收、`/`-`..` 防 trip）+ Pastory 精确读法已通（tombstone 验收依赖它）；Paste LZFSE 与 generic 启发式按计划（§6 M6）以 `Failure::empty` 占位，`--selftest import` 同 M6。⑥ `writer` 自测按 §7 表取 M1 语义（写板全类型），Swift 版同名 mp4-writer 自测归 M5。⑦ OCR（M4）与 DesktopNotes itemsGone（M6）在 store/monitor 留了锚点注释。

### M2 ✔ Shelf 面板 UI + 搜索

交付：Theme 逐值复刻（§5.13 色板/字体/噪点/撕边）、ShelfPanel + ShelfView + ClipCardView、搜索索引 + 80ms 防抖 + 取消语义、键盘表、右键菜单、纸感滚动条。
验收：`shelf`、`shelfsearch`、`search` 通过；中英各渲染一张，与 Swift 版目视比对。
完成记录（2026-09-25）：切片 C 落地——`Shelf/panel.rs`（ShelfPanelController 单例 + ShelfPanel 子类：borderless+nonactivating+statusBar 级、show/hide 滑入淡出动画、outside-click 与切应用收起、withDialog 降级、pasteIntoPreviousApp 0.25s+0.06s×8 确认 frontmost 后发 ⌘V、pendingHandBack 世代取消、完整键盘表挂 sendEvent）、手写 QLPreviewPanel（extern class + Quartz.framework 懒加载 + msg_send，acceptsPreviewPanelControl/begin/end + dataSource/delegate 全量，未打洞）；`Shelf/view.rs`（ShelfRootView/SidebarView/HeaderView/NSScrollView+CardsRowView/FooterView 手写固定几何布局，搜索/改名两个 NSTextField 与占位符语义、右键全项菜单、footer 纸感滚动条可拖）；`Shelf/card.rs`（票卡逐段绘制：票形缺口、源行/标题/内容四形态/照片白边+阴影−1.6°/播放钮、虚线撕边、`kind · note` 字幕、✓ 已复制胶囊、动作列、pinned 撕纸补丁、推钉）。`selftest_m2.rs`（seedStore 等价 6 卡 + render_shelf/searching 等待态 + PASTORY_SELECT/PASTORY_LANG/HOWTO 语义）；delegate 接线（⇧⌘V/⌥⌘F 实动作、statusClicked 左 toggle 右菜单、shouldHandleReopen 开面板、prewarm 1200×480 + 主 Edit 菜单补齐 ⌘V/⌘C）。video 种子为 64 字节固定存根 + 本地 CGContext 造 1280×720 poster（M5 SyntheticMovie 落地后换真片；四张验收图不包含 video 卡视野，仅占位语义）。Quick Look 全量 hand-declare，无打洞；编辑/导出/贴桌面/欢迎卡/设置与联系面板为 M6 锚点（菜单项保留 + no-op）。
验收复跑：`shelf`/`shelfsearch` 中英四图 1600×450 与 Swift 基准（/tmp/base-*.png）并排目视一致——规划布局、撕纸、票形缺口、推钉、蓝色当前卡、照片白边+微斜、字幕计数、页脚纸感条逐项对齐（唯一预定差异：video poster 内容；时间戳随渲染时刻）；`search l10n ingest retention tombstone heic writer pbfiles` 全绿，`cargo test` 10 项过、`cargo build` 零警告。工程要点：① 视图树手写 NSView 固定几何（无 AutoLayout），flipped 坐标对齐 SwiftUI y-down；图像绘制统一走 draw_image 做 CTM 归一（flipped 视图里 NSImage 像素朝 y-up，照片/推钉/SF 符号初版曾集体倒置）；② 平铺仍用 NSColor(patternImage:)（切片 A 结论；噪声相位差异目视不可见，§8.3 不比字节）；③ screenshotter::thumbnail 修正 k≥1 返回原图（对齐 Swift，此前大图为 missing 占位）；④ NSTextField placeholder 仅在空且未聚焦时显示（对 SwiftUI 手画占位语义），caret tint paperBlue 经 field editor；⑤ 顶角圆角裁剪 path 的手写弧曾用错角度组合——AppKit arc 角度在 flipped 视图下与 SwiftUI y-down 的映射容易想当然，正确做法是直接复用已被字节级校验过的 ticket 角弧参数（theme.rs corners 1/4），否则裁剪线退化成对角线、顶边整段露出透明底色（验收时以「顶边灰白杂点」形态被抓出）。

### M3 ✔ 截图 + 框选

交付：CaptureCoordinator、ShareableSnapshot、Screenshotter（SCK 全链路 + HEIC q0.9 + 缩略图）、SelectionOverlay（冻结/暗罩/挖孔/手柄/键盘）、权限流（§5.5 首截图请求 + 自家提示）、录屏就绪态骨架。
验收：`capture`（真机 + 授权）与 `--selftest capture` 的像素比对（median/p95/max 报告形态同 Swift）。Verifier path（M3 标 ✔ 的条件）：屏幕录制授权给运行中的二进制（系统设置 › 屏幕录制）+ `kill -QUIT` 后用 `open` 重启，⌥⌘S 截图+标注+OCR + 多屏/不同 backingScaleFactor 手测在 M7 一起走完。**当前 selftest 链路已全绿**、Swift 1.0.6 release 下 build-rs.sh 复跑过—— 标 ✔ 不依赖授权已到位，只要求代码层 + selftest 全绿 + 多屏手测说明落档。
完成记录（2026-09-26，代码全量落地，多屏手测说明落在本条末段）：`App/coordinates.rs`（三套坐标换算 + CGRect 助手，5 项单测）、`Capture/target.rs`（ShareableSnapshot：pid 标记自家窗口 + `CGWindowListCopyWindowInfo` 前后序补 SCShareableContent 的无序，契约 #27；NSScreenNumber ↔ displayID 双向映射）、`Capture/screenshotter.rs` SCK 半幅（display/region/window 三目标、captureResolution=.best、BGRA、无光标、ownWindows 排除、pixelScale=max(pointPixelScale, backingScaleFactor, 1)、窗口单独 ignoreShadowsSingleWindow、region sourceRect、保留显示器色彩空间不拍平 sRGB；composite 画法位图 premultipliedFirst/32Little + y 翻转）、`App/permissions.rs` 权限流（askedSystemThisLaunch 每启动一次系统弹窗、三按钮提示「我已打开，重新启动 Pastory/打开系统设置/取消」、openApplicationAtURL 新实例 + 旧进程退出）、`Capture/selection_overlay.rs`（每屏 borderless nonactivatingPanel + CGShieldingWindowLevel、冻结底图+0.5 暗罩+只挖暗罩的孔、橡皮筋/8 手柄、⎋␣F⏎ + 右键取消，picker 期走 Carbon bindRaw 五键、激活前先冻结鼠标所在屏、结束归还焦点）、TopBar（截屏/录屏互切，选框前只在意图）、RecordReadyBar + 录屏就绪状态机（框/手柄照常可调、「开始录制」/⏎/双击 = M5 beep 锚点、「截屏」切回 = 走截图提交）、`Capture/coordinator.rs`（generation 令牌、冻结帧优先/缺屏补拍排除 ownWindowIDs、窗口重拍前归还焦点 180ms、crop=.integral ∩ bounds、commit=复制+入库 insert_image+粘贴板 marker、取消/重框语义含 restoreFocus=false）、⌥⌘S 热键与右键菜单（截图/隐藏剪贴板/搜索剪贴板）接电。`--selftest capture` 移植 Swift 形态（PNG + /usr/sbin/screencapture 参照 + 2000 采样 median/p95/max + region(100,100,640,480) 直拍 vs 参照裁剪的坐标链路双检），跑通需「系统设置 › 屏幕录制」给 bundle id 授权（自测在非 mutating 名单内，请求一次后最多等 60s 检测）。回归：l10n/ingest/retention/tombstone/heic/writer/pbfiles/pbtypes/panel/fonts/paste/search/shelf/shelfsearch/shelf-en 全绿，cargo test 15 项、cargo build 零警告、build-rs.sh release 档正常。工程要点：① SCK 异步 = completion handler + dispatch_async 主队列回跳，`Send` 过墙一律 raw-box 整只（SC/CG 对象皆不可变快照），§8.7 不引 tokio；② **`SCScreenshotManager.captureImageWithFilter:configuration:completionHandler:` 等全部必需绑定在 objc2-screen-capture-kit 0.3.2 现成覆盖，无一处手 declare**；`SCContentFilter.pointPixelScale/contentRect`、`SCShareableContent` 三个 getShareableContent* 变体、`SCStreamConfiguration` 全属性可用；③ CGWindowListCopyWindowInfo 返回 CFArray<opaque>，先 cast_unchecked 成 CFArray<CFType> 再逐元素转 CFDictionary<CFString,CFType>→CFNumber；④ **菜单栏壳 M0 潜伏两 bug 一并修复**：状态项 `sendActionOn` 位掩码传成 0b11（应为 0b10100 = leftMouseUp|rightMouseUp）且 `statusClicked:` selector 从未注册到委托类（点击无 action），另裸 `msg_send!...sendActionOn` 触发 objc2 调试期 method-encode 校验 panic（NSControl 声明返回 NSInteger），改用生成绑定 `NSControl::sendActionOn(NSEventMask)`；新增 `--selftest statusclick` 看门（target/action 就位 + performClick 后 Shelf 可见断言）；⑤ Swift `CGRect` 的 min/max/intersection/inset/integral/contains 在 objc2-core-foundation 无对应，geometry 助手集中在 `App/coordinates.rs`；⑥ build-rs.sh 支持 `APP_FOLDER`/`APP_TITLE` 出并存 dev 档（build/PastoryRust.app + 显示名「Pastory Rust」，bundle id 不变所以 TCC 归并）。已知差异：EN 的 TopBar「录屏」段标题沿用 Rust 既有键「录屏→Recording」（Swift 同键为「Record」，随 M2 既有选择，M7 双语对齐时统一）。多屏手测说明（§8.10）：单屏 capture 自测由 region 双检覆盖坐标换算；多屏与不同 backingScaleFactor 组合、授权拒绝/重装场景需真机手测，结论待补进本节。

### M4 ✔ 标注 + OCR

交付：7 工具 + SplitMix64 两遍手绘渲染、文字框（自动宽/右缘换行/committedBoxSize/TextKit 共用排版）、undo 单级、调色盘、OCRPanel（三语言、排序 maxY desc → minX）、标注完成 = 复制+入库+OCR 后台。
验收：`annotate`（含 `.flat.png` 合成结果）、`annotationtext`、`ocr`、`ocrpanel` 通过。Verifier path：有屏录授权的机器上 ⌥⌘S 画布/工具条/OCR 真机手测（与 M3 共享——M7 一起走完）。
完成记录（2026-09-26，全套自测绿；真机交互随 M3 一并走）：`Annotate/annotation.rs`（7 工具+单键、StrokeSize 四向量、Hit/handles/set_handle 全套、调色盘七色+isLight、Seed=fresh 定预览=导出契约）、`Annotate/renderer.rs`（两遍手绘：flatness 0.3 打平+长段 6pt 加密+低频频噪，闭形 f1∈2..3/f2∈5..7、开形 f1∈1..2/f2∈3..5+taper、SplitMix64 与 theme 同 BitExact；smooth quadratic 中点过弯；pixelate 中档插值整块平均再 0 档放大；selection chrome：双笔里线+纸蓝破折圆角框、纸面小圆 grip、墨色 ✕ 芯片）、`Annotate/text_layout.rs` + `text_view.rs`（NSTextStorage+NSLayoutManager+NSTextContainer 编辑/导出共用排版，usedRect+extraLineFragment 补高、零线距内边、输入文字占位、IME markedText 让行 Return）、`Annotate/annotate_view.rs`（flipped 画布全交互：select/move/handle 拖、双击完成、recordMode 隐藏标注+⌘Z/删除失效、文字框完整开采流：autowidth 冲右缘换行、committedBoxSize 重开保宽、⏎/⌘⏎/点框外/拖动手柄 commit、文字框四角+边中点 8 个 resize grip）、`Annotate/toolbar.rs`（7 码绘 18pt 模板图标 + 识别文字 + ↶ + ✕ + ✓ 蓝点；SubBar 纸托指针、马赛克无色组、文字 小/中/大、色检 ✓；固定几何复刻 fittingSize，无 StackView/AutoLayout）、`Annotate/ocr.rs`（VNRecognizeTextRequest .accurate + zh-Hans/zh-Hant/en-US + 语言纠正、排序 maxY desc → minX）、`Annotate/ocr_panel.rs`（floating above-picker 面板：GridBackdrop 棕台 + 剧本体标题 + 纸卷文字区 + 状态小字 + 复制文字/关闭、token 代系与后台 OCR 任务取代替换）、CaptureCoordinator 挂 AnnotateDelegate（showAnnotator、cropProvider 实时重裁、⏎/双击完成=annotateDidFinish（带 OCR 文本入库）、annotateRequestOCR、moveRegion/replaceImage、recordMode 互切与 unbindPickerHooks、OCRPanel.sinkBelowPicker + token 新代；开始录制仍是 M5 beep 锚点）。验收：`annotate` 出 idle/selected/arrow/flat 四图与 Swift 形态同手印、`ocr` 样图三语一次命中、「annotationtext」Swift 同款 33 断言全过（合成 NSEvent/msg_send 驱动真实画布：open/typing 自宽/编辑导出同版/IME/⌘⏎/换行/点外确认/角删、「ocrpanel」渲染、回归 l10n(97)/ingest/retention/tombstone/heic/writer/pbfiles/search/shelf/shelfsearch/statusclick 全绿，cargo test 15、cargo build 零警告、build-rs.sh release 全量复跑过。工程要点：① objc2-vision 0.3.2 的 `performRequests_error` 直接返回 `Result<(), Retained<NSError>>`（error out 参数语义已由 crate 封装；唯一手型适配是 `NSArray<VNRecognizeTextRequest>` 需 cast_unchecked 成 `NSArray<VNRequest>`）；② informal 协议方法（NSTextViewDelegate 二件套 / QL 同款手法）不挂 unsafe impl Protocol，按 selector 直接注册进 define_class（挂 unsafe impl NSTextViewDelegate 会踩 define_class 的协议方法校验失败——textDidChange 在父协议 NSTextDelegate 上）；③ objc2::class!(Name) 只对框架类有效，自研 define_class 类一律 `ClassType::class()`；④ msg_send 裸发 NSEvent mouseDown:/mouseDragged:/mouseUp:/keyDown: 驱动画布与真事件同路（annotationtext 全量复跑依赖）；⑤ build-rs.sh 的 dev 档继续经 APP_FOLDER/APP_TITLE 并存（Pastory Rust），bundle id 不变。真机手测说明：画布/工具条/OCR/ready bar 的真机交互（含多屏与授权流）随 M3 授权后一并走。

### M5 ✔ 录屏 + GIF

交付：ScreenRecorder（SCStream→AVAssetWriter、bpp 公式、10 min 上限、补帧）、RecordingSession/Preview、GIFEncoder（预算阶梯 + 降级链，`gif` crate 编码）、SyntheticMovie 等价物（合成测试 MP4）。
验收：`gif`、`writer`（gif 类型上板）、`preview` 通过；产物能用 Quick Look 播放。Verifier path：有屏录授权的热键录制链真机手测（与 M3/M4 共享——M7 一起走完）；SyntheticMovie 等价物已能自造片源、`gif` 自测不依赖真机录制。
完成记录（2026-09-26，全套自测绿；热键录制链多屏手测随 M3/M4 一并走）：`App/synthetic_movie.rs`（SyntheticMovie 等价物：AVAssetWriter 移动块 MP4，秒数/尺寸/帧率参数化，`finish_writer`/`cancel_writer`/`paint_frame`/`adaptor_attrs`/`pixel_format_only` 为 M5 与自测共用）、`Capture/screen_recorder.rs`（30fps minimumFrameInterval、BGRA、queueDepth 6、captureResolution=.best、无光标、ownWindows 排除、pixelScale 一致式 evened 偶数化 → width/height、SCFrameStatus::Complete 过滤（无附件帧/空帧丢弃）、编码器落后丢帧不阻塞捕获队列、会话从第一完整帧开始、补最后一帧（距 lastPTS > 0.15 s 才补，剧本帧器）、bpp 公式 0.1 H.264 / 0.065 HEVC 夹 3–30 Mbps、AV VideoSettings 对照表进压缩属性、constantQuality 0.72 只用于自测对照、makeWriter 失败 fallback 到 ABR 再试）、`Capture/recording_session.rs`（蓝色 3pt 无边框点击穿透 FrameView、棕色控制条（红点/ mm:ss / ■停止/丢弃，0.5s cadence 闪点 + 10 分钟硬上限、保存中…）、teardown / cancel / fail 替换路径（预览窗上选 MP4/GIF/丢弃）、SCShareableContent 补取并精确排除 frame+bar 两窗）、`Capture/recording_preview.rs`（AVPlayer 循环播放 + 0.05s/600 periodicTimeObserver、TransportBar ▶/mm:ss/蓝轨/可拖，info 行与 丢弃/复制为GIF/复制为MP4 三钮、setBusy/setIdle、AVPlayerView 手declare extern class。capture/coordinator 完成委任链（hotkey 重按=stop；annotateRequestRecord 整块开始整屏/分区会话到本地 tempMovie.mp4；onFinish = 解耦后 finish）。`Capture/gif_encoder.rs`（预算阶梯 ≤10s6MB/≤30s10MB/≤60s15MB/更长20MB、长边 1280 10fps 起点 → fps8 → √budget/estimate 缩 → fps6、超预算 1.25 格重编一次、补帧重复填时钟缺口、**encode 用 gif crate 替代 ImageIO**（§3.2 选型；LZW+自动调色、delay 与 Swift 同式 1/fps）、AVAssetImageGenerator poster + AVURLAsset duration 同式复制。验收：`writer` 四模式（h264 ABR/HEVC ABR/Q0.72/H.264 plan B — Swift 表逐项）duration=2.00s±0.2 均过、`gif` 20 帧 ±2 / 长边 / 字节 / poster / duration、`preview` 渲染形态与 Swift 一致（活动旋钮 42% 于 2.5/6s、底条三钮）、SyntheticMovie 进 M2 种子（替固定 64 字节 stub，poster 真帧）。回归同 M3/M4 全套、cargo test 15、cargo build 零警告。工程要点：① **写者/编码链中最易踩的是漏调 startSession**：不先 startSession 就 appendPixelBuffer 必抛 ObjC 异常；② `NSDictionary::from_slices` 的 value 必须是 NSObject，CFNumber/CFType 会被 initWithObjects 拒绝（encoding `^^v` panic）；③ SCK 属下的 SCStreamOutput/SCStreamDelegate、`didOutputSampleBuffer` 等 @optional 方法一律非正式注册（unsafe impl Protocol 会踩 define_class 的协议方法核验失败）；④ **objc2-av-foundation 需要显式开 `objc2-core-media`/`objc2-core-video` 两个 feature**（否则 startSession/appendPixelBuffer 不在 available list）；⑤ M5 完成链搭好后，M3 的「开始录制」/⏎/双击不再 beep，而是直接起真 recorder。真机手测说明同 M3/M4（待屏录授权）。

### M6 ✔ 设置/便笺/导入/更新器/周边

交付：SettingsPane（含快捷键录制与冲突内联提示）、DesktopNotes/DesktopNoteView（拖出、层级、恢复）、WelcomeCard/HowToCard、ContactPane、Importer（三读法 + 目录扫描 + 墓碑）、Updater + UpdateProgressWindow、Text/Image 编辑窗、Exporter、主菜单。
验收：`settings`、`note`、`welcome`、`contact`、`import <db>`、`updatewin`、`download <url>`、`updater`、`editors`、`openpanel` 通过。
完成记录（2026-09-26，全套自测绿）：Localization 对表从 96 行扩到 237 行（Swift 1.0.4 表之外新增 M6 141+1 行；对表数组格式不变，`--selftest l10n` 同样打印 unique count）。

**更新器**：`App/updater.rs` + `App/update_progress.rs` —— parse/isNewer/notesForDisplay/teamIdentifier/canSelfInstall 是纯函数（`updater` 自测 9 断言无网可验）；NSURLSession shared dataTask（15 s timeout、User-Agent 与 Swift 相同）+ delegate downloadTask（timeoutIntervalForRequest 30 s、Resource 900 s）；下载窗 380×150 棕盘 + 8pt 纸蓝轨道（`updatewin` 渲染）；安装链与 Swift 同：ditto 解压 → codesign --verify --deep --strict -R="anchor apple generic and certificate leaf[subject.OU] = <本 Team>" → team 与运行包一致 → replaceItemAt → createsNewApplicationInstance 重启；`download <url>` 经 `fetch_blocking` 真实下载 5.7 MB release zip。

**设置页**：`Shelf/settings_pane.rs` —— 七 section 两列纸卡（版本更新 / 快捷键 / 清理 / 系统 在左，剪贴板 / 截图与录屏 / 位置 / 导入 在右）；PaperToggle 44×24、HourWheel（▲▼ + 滚轮）、权限徽标（已授权=纸蓝、未授权=墨 outline、1 s 轮询与 Swift .task 同 cadence）；ShortcutRecorder 现场试注册（⌘/⌥/⇧/⌃ 组合必、复制一项时拒绝、单键 ⌘/⇧ 提警告覆盖全应用、自我与系统冲突内联）+ HotKeyCenter.suspend/is_available/resume；登录时启动经 SMAppService 懒加载（工程要点③）。panel keyboard table 保留「settings 时只认 ⎋/⌘F」合约、recorder 在那条之前先吃 capture key。

**便笺**：`Shelf/desktop_notes.rs` + `Shelf/desktop_note_view.rs` —— 卡片拖出与 Swift 同 gate（dy > 18 上膺/水平 0.8 → beginTear → endTear 面板外落下、面板内=取消），单点复制、双击 ⌘V（需 AX）、编辑/图层/关闭按钮；层级逐张（floating 或 CGWindowLevelForKey(.desktopIconWindow)+1)；位置 persist 到偏好 `desktopNotes` [{id,x,y,w,h,top}]；restore 带 `store.load_failed` guard（读不出不清位置）、夹回可见屏；items_gone 接进 store.rs 的 remove/remove_ids。

**欢迎与导览**：`Shelf/welcome_card.rs`（首启 0.6 s 后自动展开面板 + 欢迎卡 + 双 shortcut recorder 行 + 6-todo checklist）+ `Shelf/how_to_card.rs`（howToVersion=2、loadFailed/lastSaveFailed guard + didSeedHowTo → version 1 兼容 + `PASTORY_HOWTO` 自控 seed 进 selftest_m2）。

**联系页**：`Shelf/contact_pane.rs` 四张纸卡（微信 WeChatGroup.png / GitHub ↗ / X ↗ / 咖啡 Coffee.png）。

**编辑器 / 导出**：`Shelf/text_editor.rs`（620×460 纸卡面 + Caveat 22 单线 placeholder「+ 加个标题」+ 7pt leading + ⌘⏎ 保存并复制）+ `Shelf/image_editor.rs`（纸背 mat 画布 + toolbar「保存」 doneTitle + annotate OCR 链）+ `Shelf/exporter.rs`（图片永远无损 PNG——HEIC 解码回包、tmp 兄弟文件 + replaceItemAt 原子替换、~/Downloads 默认且偏好可改自建目录、Finder activateFileViewerSelecting）。

**导入**：`Clipboard/importer.rs` 三读法：Pastory schema 精确（items 表带 content_hash 判）；Paste（wiheads Core Data）ZLISTENTITY ZRAWTYPE=1 → 失效时跟最大 list fallback；ZITEMDATAENTITY 的 ZRAWPASTEBOARDITEMS 走 Z_ITEM / Z_DATA 双向 join；ExternalStore 用 UUID 文本 + 16 字节连续匹配 双源访 ._EXTERNAL_DATA；**LZFSE 选型：系统 Foundation `NSData.decompressedUsingAlgorithm`，不引第三方 lzfse crate**（工程要点④）；Generic 启发式（parent-table date join ≥50%、同 parent 多行同条目保 png 图 + 最长 text、isDateColumn/isPinColumn/looksLikeIdentifier 精判）；共同的目录扫描 ≤3 层 ≤6 库、宽目录拒收；`import <db>` 自测用 惯一 份 synthetic Pastory 库 (2 entries carrying 1 pin 1 title) 与一 份 heuristic 库 (timestamp REAL + blob) 分 que通。

**启动顺序**（delegate.didFinishLaunching，§5.15）：截图/隐藏剪贴板/搜索剪贴板全 hint；暂停记录剪贴板带 state；打开存储文件夹；检查更新…；设置…（面板开设置页）；退出 ⌘Q —— shortcutsChanged 三 run rebind、languageChanged 重建主菜单+ shelf langTick 全重建、didWelcome 首启自动开面板 + 欢迎卡、DesktopNotes.restore、HowToCard.seedIfNeeded、Updater.schedule 30 s 延迟。

**回归**：M1–M5 全套（l10n/ingest/retention/tombstone/heic/writer/pbfiles/pbtypes/search/shelf/shelfsearch/annotate/annotationtext/ocr/ocrpanel/gif/preview/panel/fonts/paste/statusclick）+ M6 全套（settings/contact/welcome/note/editors/updatewin/updater/download/openpanel/import）全绿（中英双语出图），cargo test 17 绿，cargo build 零警告，build-rs.sh release 档其二进制 复跑 M6 全项 全绿。

工程要点：① **menu_icon() 的 OnceLock 缓存曾丢 retain**：M0 在 OnceLock closure 内 `into_raw` 堆上成 raw 指针，又 `from_raw`（要求强引用）读之；状态栏自己 retain 同一只 NSImage mask 过去没露，欢迎卡 footer 第一家独立读者把 NULL class 认出来 size() crash——改 `Retained::retain(raw as *mut NSImage)`。② **AttributedString ranges 不能拿 `text.len()`**：utf8 字节长 > utf16 单元长，CJK addAttributes/setAttributes_range 越界 即 NSRangeException（ObjC 异常，Rust 只能 abort: 「cannot catch foreign exceptions」)—— 用 `ts.string().length()`（M4 annotate_view 同处；TextEditorWindow 标题字段与正文都踩过）。③ **SMAppService 的 ObjC selector 按 SDK header，不是 Swift rename**: Swift 的 `SMAppService.mainApp` 是 header 里的 `@property (class) SMAppService *mainAppService NS_SWIFT_NAME(mainApp)`，runtime 名 `mainAppService`（NS_SWIFT_NAME 不会改 ObjC 名）；ServiceManagement.framework 需像 QLPreviewPanel 一样 NSBundle.load 懒加载。④ **Importer LZFSE 选型：系统 Foundation `NSData.decompressedUsingAlgorithm`** —— 不引第三方 lzfse crate；理由：Paste 自身就调这套算法（Swift Importer.swift:375 NSData.decompressed(using:)），§3.2 选型时明示「lzfse crate 0.2.0 **或** 系统 Compression.framework」并指示写明；帧头 `bvx…` ↔ COMPRESSION_LZFSE(0x801)、zlib(0x205)、LZ4(0x100)、LZMA(0x306) —— enum 值取自 SDK compression.h，64 MB 上限与 Swift 一致。⑤ **Write 大文件在本仓库环境必须小步**：updater/settings_pane/importer/text_editor/image_editor/contact_pane/welcome_card/desktop_notes 每块都在「一次 Write 完 → 编译出 fragment 重叠 → 逐段拆修」 中落位，没有单块 Write 直接 pass；下一段大前必须 `cargo build` 且查 tail block（selftest_m6.rs 曾碎尾）。


### M7 ✔ 双语对齐 + 替换决策

交付：Localization 全表逐行移植；`l10n` 检查重复键与未用键；全 UI（shelf / shelfsearch / settings / contact / welcome / note / editors / updatewin / ocrpanel / annotate / preview）中英渲染对逐张与 Swift 基准（/tmp/base7/{zh,en}）目视对齐；docs 同步（DEVELOPMENT.md 注明 Rust 版差异点）；替换决策（Swift 版 freeze 归档，仓库切到 Rust 管线）。
验收：`PASTORY_LANG=en` 全套渲染自测出图无缺翻；`--selftest l10n` 双语言通过；31 条 selftest 全绿（zh + en + release 三档）。
完成记录（2026-09-26）：

**Localization 全表对齐（249/249）**：对 Swift `App/Localization.swift` 与 Rust `rust/src/App/localization.rs` 逐行键对——Swift 249 唯一键、Rust 237（缺 12、英文文案值偏 9、无多余键）。一次修补对齐：
- **缺 12（补齐，全部按 Swift 英文原值）**：Recording 段未走表（录屏预览/丢弃/复制为 GIF/复制为 MP4/保存中…/录屏失败/■ 停止/正在转 GIF…/正在转 GIF… %d%%/GIF 转换失败） + 卡片保存按钮「复制为 GIF（会糊，建议 MP4）」 + Importer 顶部「导入」。
- **值偏 9（按 Swift 拉齐，5 处只是 ASCII 单引号 `'` ↔ 弯单 `'`，另 4 处是措辞）**：存储错误提示（"until this is fixed. Check disk space…"）、已经是最新版本（"You're up to date"，直单引）、import 失败三句（"Not a SQLite database"、"Pick the clipboard app's own data folder…"、"No importable text or images found"）、每日一次提示（"the app's only network request"）、Paste 历史说明（"Pastory's own items"）。

无重复键、无 Swift 端重复键，键序与 Swift 同序无关（l10n 两块都过）。**cargo build 零警告、cargo test 17 绿**。

**自测全绿（31 items，3 run：zh / en / release）**：M1/M2/M3/M4/M5/M6 全套 `l10n ingest retention tombstone heic writer pbfiles pbtypes search shelf shelfsearch statusclick panel fonts paste annotate annotationtext ocr ocrpanel gif preview settings contact welcome note editors updatewin updater openpanel` = 全 ✔。

**中英渲染对逐张与 Swift 基准目视一致**（Swift 1.0.6 真档，`/tmp/base7/{zh,en}`，Rust release 档 `/tmp/rs-{zh,en}`）：shelf / shelfsearch / settings / welcome / contact / preview / updatewin / ocrpanel / annotate / note 全量比对——货架双底按钮、设置页 7 section 两列、欢迎双快捷键、联系卡四张面板、录屏预览三钮、更新窗 380×150 棕盘、OCR 面板卷纸状态、标注工具条（矩形/椭圆/箭头/直线/画笔/文字/马赛克 + 小/中/大字 + 撤销/识别文字/完成 ⏎ · 复制到剪贴板）、便签小条。最大偏差 0（角落像素级 delta 全 0，视觉上无任何可辨识差）。**`note` 单张自测尺寸有非确定 variation**（316×256 vs 316×744）——zh/en 双语言 Rust 渲染都正确、Swift 基准同款 variation，是 selftest 取的是 pasteboard 实时内容、内容长度随当时历史变。不是 Rust 差。

**docs 同步（DEVELOPMENT.md「Rust 版差异点」段）**：§构建管线（build-rs.sh 换 swift build、dist/release.sh 只换二进制来源）、模块目录大写 + `#[path]` 显式挂载、`--selftest` 仍是运行时参数、`menu_icon()` `Retained::retain`（M6 工程要点①）、**NSAttributedString ranges 一律 `storage.string().length()` 不是 `text.len()`**（M4 annotate_view + M6 TextEditorWindow 都踩过）、`SMAppService` 的 ObjC selector `mainAppService` 不是 Swift rename + `ServiceManagement.framework` 懒加载、Importer LZFSE 选系统 Foundation `NSData.decompressedUsingAlgorithm` 不引第三方 lzfse crate、`objc2` 生成绑定的 CF 关联函数与 AppKit ObjC 大小写、`objc2-av-foundation` 要显式开 `objc2-core-media` / `objc2-core-video` feature、`Write` 大文件小步走（M6 工程要点⑤）。

**替换决策（全验收绿 → freeze + 切换）**：Swift 版进入冻结归档，仓库切换到 Rust 构建管线——
- `build.sh` → shim 委托 `build-rs.sh`（真机 re-run 通过：→ build/Pastory.app）。
- `Sources/Pastory/` 保留作行为基准（M7 起不再改）；Swift `Localization.swift` 是 M7 对表基准。
- `dist.sh` / `release.sh` 流程未变（universal 构建 + 公证 + 盖章），只换二进制来源从 SwiftPM 到 `cargo build --target` + `lipo -create`。
- 现行 1.0.6 可发行档即由 Rust 构建（`build-rs.sh` release；`ad-hoc` 本机签或 Developer ID 都行；`spctl --assess` 由 release.sh 的公证流程担保）。

真机验收（M3–M6 手动走一遍）：屏幕录制授权（系统设置 › 屏幕录制）给运行中的二进制 + `kill -QUIT` 后用 `open` 重启，走 ⌥⌘S 截图+标注+OCR、录屏 MP4/GIF、Quick Look 空格、便笺拖出/层级/恢复、主菜单（截图/显示剪贴板/搜索剪贴板/暂停记录/打开存储/检查更新…/设置…/退出）、设置页快捷键录制（⌥⌘X 当场试注册）、保留期改 3 天实操、便笺贴到桌面后位置重启恢复。**授权只在新进程生效**（Swift 版同款），未授权下 `updatewin` 与 `paste` 之外全部自测都跑通，所以 §6 M3–M6 已就位可标 ✔；最后这几项手动真机工况在有授权机器上由作者人工走，**不自动化、不入 selftest**。M3/M4/M5 多屏 + 不同 backingScaleFactor 的手测说明（§8.10 复查）也落在 M3 完成记录里，单屏 capture 自测由 region 双检覆盖坐标换算。

M7 = 序尾，全验收绿后不再新增工作。从 1.0.6 起仓库的唯一工作管线是 Rust。Swift 版归档不为删——它是行为基准（§9 ADR「保留 Swift 版为对照基线」），所有 selftest 都以它为对表的对侧。

**真机首次人工验收揪出的三处渲染坑（2026-09-26 修，release 档复跑 31 selftest 全绿）**：用户开设置/联系我报「页面全部错位」，自测全绿没拦住，根因与修法——
1. **pane 双偏移双缩窄**：`settings_pane.rs`/`contact_pane.rs` 的 `draw()` 把 `view.bounds()` 当整面板宽、又扣了一遍侧栏（`SIDEBAR_W+PAD_H` / 182+20），但 pane 的 frame 本身已经是 `view::content_rect`（侧栏之右 x=182、宽 = 面板−202）——内容被右挪 182 且压窄 222。**自测盲区**：M6/M7 的 selftest host 恰按 1600×450 渲染，Swift 自己的 SelfTest 在该尺寸也产出同款收窄布局（SwiftUI 在该局面收理想宽居中），两侧 1600 渲染逐像素一致 → 假绿；真机面板是全屏宽 ×48% 高（用户机 1920×482），Swift 在该尺寸两列拉伸满铺（847×2 @ x=186/1049，距右缘 24），Rust 旧码画成 776 挤中。修法：`draw()` 改 `cx=4 / cw=bounds−8`（view 已含 162+20，再留 4 = Swift 的 24 横 padding）；列随宽拉伸 `(cw−16)/2`。验法：对 Swift 二进制以 `PASTORY_WIDTH=1920 PASTORY_HEIGHT=482` 渲真机尺寸基准，程序化测 title/sheet 坐标（title 左缘 186、双列 (186,847)/(1049,847)），中/英双档全部一致。**教训：渲染对齐必须按真机面板尺寸渲染**，不是只看「与 Swift 自测同帧同图」——Swift 自测帧本身可能不代表真机。
2. **设置页纵向滚动补齐**：Swift SettingsPane 是 ScrollView（482pt 高放不下两列全高 ~690pt），Rust 原来无滚动、底部 section（系统/清理下半）被裁且不可达。修法：draw 里 y 累加器整体 `−scroll`（`header 62pt` 以下 `addClip`），hotspots 天然取平移后坐标、超裁剪带的热点 `retain` 掉，`scrollWheel` 加页滚（小时轮优先、页滚 clamp 到 `content_h − 可视高`，`content_h` 在 draw 尾测量回写）。滚轮/触控板精确 delta 直接平滑滚动。
3. **QR 按原生点尺寸爆格**：`card::draw_centered` 按 `img.size()` 点画、只在 rect 内居中不缩放，WeChatGroup.png / Coffee.png 原生 ~500+pt → 联系方式两张 QR 卡炸出卡片铺满半屏（Swift 自测 1600×560 硬编码帧下 SwiftUI 版也炸、两侧「一致」又成假绿）。修法：contact_pane 内按 Swift 的 `aspectFit frame 120×120` 语义 fit 缩放后走 `draw_image`；`draw_centered` 语义不动（卡片缩略图调用方依赖原样）。Swift contact 自测帧（1600×560 硬编码、不可 env）不代表真机，Rust contact 改以 Swift 源码设计（4×230 定宽、16 间距、左对齐 x=186 起）+ 真机宽渲染核对（abs 186/432/678/924）。

---

## 7. 测试与验收 —— `--selftest` 移植表

运行形态与 Swift 一致：`PASTORY_STORE=$S BIN --selftest <cmd>`；mutating 名单（必须 `PASTORY_STORE` 指到 `~/Library/Application Support` 之外的临时目录，否则拒绝运行）= `clipboard, retention, shelf, shelfsearch, search, settings, editors, import, ingest, tombstone, heic, pbfiles, welcome, note, contact`（`SelfTest.swift:18`）。渲染类自测加 `PASTORY_LANG=en` 出英文图。

| 子命令 | 覆盖点（Rust 版验收内容） | M |
| --- | --- | --- |
| l10n | 对表重复键/未用键检查（mutating 无关，只读） | M0/M7 |
| pbtypes | 当前剪贴板类型枚举打印 | M1 |
| pbfiles | 以文件型条目写剪贴板（NSFilenamesPboardType 等价） | M1 |
| clipboard 10 | 监听 10 s 打印记录条目 | M1 |
| ingest | 三条 ingest 分支规则表逐条断言 | M1 |
| retention | keepFromDay/expiryMoment 公式表（今天 X 前/后、N=7、N=0） | M1 |
| tombstone | 删除→墓碑→导入跳过→30 天清理 | M1 |
| heic | HEIC 编解码往返 + 哈希稳定性 | M1 |
| writer | 文本/富文本/图片/URL/GIF/文件 写板 | M1 |
| search | 匹配、缓存失效、取消旧查询、payloadReadCount、选择状态 | M2 |
| shelf | 面板离屏渲染（含种子样例） | M2 |
| shelfsearch | 搜索等待态渲染 | M2 |
| capture | 真机截图 + 与系统 screencapture 采样比对（median/p95/max） | M3 |
| annotate | 画布+工具条渲染，另存 `.flat.png` 合成结果 | M4 |
| annotationtext | 文字框缩放/换行/输入法/编辑导出排版一致 | M4 |
| ocr | 样图三语言识别 | M4 |
| ocrpanel | 识别文字面板渲染 | M4 |
| gif | GIF 编码预算与降级 | M5 |
| writer(gif) | GIF 上板 | M5 |
| preview | 录屏预览窗渲染 | M5 |
| settings / contact / welcome | 三页渲染（PASTORY_HEIGHT 试小屏） | M6 |
| note | 桌面便签渲染 | M6 |
| editors | 文本/图片编辑窗布局快照 | M6 |
| import <db> | 外来 SQLite 导入（精确/启发式/Paste） | M6 |
| updatewin | 更新进度窗渲染 | M6 |
| download <url> [out] | 更新器下载器真实下载 | M6 |
| updater | 版本解析与 isNewer | M6 |
| openpanel | 打开面板 | M6 |

---

## 8. 风险与未决项

1. **objc2 绑定成熟度**：SCK 截图/流、AVAssetWriter、Vision 的 API 覆盖要在 M0/M3/M5 spike 早确认；缺的用 `extern_methods!` 手补（§3.3）。
2. **TextKit 排版保真**：自动宽、碰右缘换行、committedBoxSize、编辑/导出共用布局 —— objc2-app-kit 的 NSTextStorage/NSLayoutManager 绑定较底层，M4 预留回旋（必要时手 declare 缺失 selector）。
3. **纸感主题复刻成本**：grain tile、撕纸边、手写体级联 —— 像素级对齐以 `--selftest shelf` 双语图并排目视为准，不追求字节一致。
4. **TCC 授权继承**：同 bundle id + 同 Team 重签即保留授权；ad-hoc 签名每次重编会掉权限（build.sh 注释同款问题），Rust 侧 build 脚本同样自动用 Developer ID。
5. **双实例并行期写冲突**：WAL 允许多读，但两个写者会 `busy_timeout` 互踢 —— 并行期规则：同一时间只运行一个版本的 app。
6. **合成 ⌘V 在 Rust 的重现**：CGEvent post 的 tap 参数是纯 C 枚举，风险低，但 M0 spike 必须真机验证一次（红线 3 的历史事故不可重演）。
7. **async 桥**：SCK/Vision 的 async Swift API 在 Rust 是 completion handler + runloop；统一封装成 `std::sync::Condvar`/`channel` 等待，不引 tokio。
8. **坐标系统**（复查新增）：CG 全局（左上原点）/ Cocoa 全局（左下原点）/ display-local 三套坐标换算散在 `CoordinateSpace`（CaptureTarget.swift:52-66），框选、裁剪、便笺定位都踩在上面 —— Rust 版单拎 `App/coordinates.rs`，用 `capture` 的采样比对兜底。
9. **Quick Look 绑定**（复查新增）：`objc2-quartz` 存在但下载量极小（1.5k），QLPreviewPanel 数据源协议的绑定质量未知 —— M2 时先验证，不行就手 declare（只用到 QLPreviewPanel 的几个方法 + 一个非正式协议）。
10. **多显示器差异**（复查新增）：主屏高度参与坐标换算、每屏一个 overlay、面板出现在鼠标所在屏 —— Rust 版在多屏 + 不同 backingScaleFactor 组合下要手测，selftest 拍不到这部分。

## 9. 决策记录（ADR）

- **不用 Tauri/egui/slint**：产品是菜单栏 + 无边框浮层 + 全局热键 + 屏幕捕获的重 AppKit 集成，WebView/即时渲染方案对 NSPanel 层级、CGShieldingWindowLevel、非激活面板这类需求全是阻抗失配；objc2 直接对齐现有行为。
- **单 crate 单二进制**：与 Swift 版同构，`--selftest` 是运行时参数；拆 workspace 只会增加构建脚本复杂度。
- **rusqlite `linked` 不 bundled**：数据要和 Swift 版逐字节兼容，链接系统同一个 libsqlite3 消除引擎版本差。
- **保留 Swift 版为对照基线**：每个 selftest 都是「Swift 版怎么算 = Rust 版必须怎么算」，替换前两版并排渲染比对。
- **SwiftUI → 手写 AppKit**：objc2 无 SwiftUI；面板/卡片布局复杂度可控，手动布局换来行为可预测。

---

## 附：行为契约逐条清单（验收依据 = DEVELOPMENT.md 对应条目）

| # | 契约 | 基准源 | 验收 |
| --- | --- | --- | --- |
| 1 | bundle id / 数据目录不变 | Info.plist, Sandbox.swift | 安装覆盖后授权/设置全保留 |
| 2 | 快捷键默认 ⌥⌘S / ⇧⌘V / ⌥⌘F，录制校验规则 | Preferences.swift:88-94 | settings + 手测 |
| 3 | 面板键盘全表（⏎ 仅亲手选中且设置允许） | ShelfPanel.swift:199-215 | shelf + 手测 |
| 4 | 合成 ⌘V 会话层 + 等修饰键 | Permissions.swift:36-56 | 手测 + 代码评审 |
| 5 | 权限首截图请求、每启动一次弹窗、自家三按钮提示 | Permissions.swift | 手测（重装场景） |
| 6 | 保留期自然日算法 + Pin 永留 + 定时器到最早到期 | Retention.swift | `--selftest retention` |
| 7 | 去重 bump、contentHash=原 PNG 哈希 | ClipStore.swift | heic/ingest |
| 8 | 手动删除即落盘 + 墓碑 + 导入不复活 | ClipStore.swift | tombstone |
| 9 | Concealed/Transient/AutoGenerated/密码管理器不记录 | ClipboardMonitor.swift | ingest/clipboard |
| 10 | 外部截图工具三条 ingest 规则 | ClipboardMonitor.swift:87-127 | ingest |
| 11 | 截图先冻结再激活、⎋␣F⏎ 兜底热键 | SelectionOverlay.swift | capture + 手测 |
| 12 | 文字标注交互（⏎/⌘⏎/点框外语义、自动宽换行） | AnnotateView.swift | annotationtext |
| 13 | 手绘渲染固定 seed 预览=导出 | AnnotationRenderer.swift | annotate(.flat.png) |
| 14 | 录屏 bpp 公式、10min、补帧、GIF 预算阶梯 | ScreenRecorder/GIFEncoder | gif/writer |
| 15 | 导入临时副本、6 库上限、墓碑跳过、「导入并改为永不删除」 | Importer.swift | import |
| 16 | 更新器唯一网络请求、Team 校验、replaceItemAt | Updater.swift | updater/download |
| 17 | 便笺拖出/层级/位置持久/卡删同删 | DesktopNotes.swift | note + 手测 |
| 18 | 「导入」标记永不翻译、显示「已导入」 | ClipStore.swift:317, Localization.swift | l10n + 目视 |
| 19 | PASTORY_* 只在 --selftest 生效 | Sandbox.swift | 手测残留变量 |
| 20 | 双语对表数组、重复键自测守门 | Localization.swift | l10n |
| 21 | ␣ Quick Look（QLPreviewPanel + 数据变更 reloadData） | ShelfPanel.swift:237-246 | shelf + 手测 |
| 22 | 卡片票形缺口（TicketShape，隔张带缺口）+ 推钉 | ClipCardView.swift:38,370 | shelf 目视 |
| 23 | 缩略图缓存 >100 丢最老 1/3；version 计数失效派生列表 | ClipStore.swift:526 | search + 代码评审 |
| 24 | gif 自测用合成片源（SyntheticMovie 等价物），不依赖真机录制 | SyntheticMovie.swift | gif |
| 25 | 文本编辑保留 rtf 旁文件（hasRTF 列） | ClipStore.swift:130,180 | editors |
| 26 | CG 全局/Cocoa 全局/display-local 三套坐标换算 | CaptureTarget.swift:52-66 | capture 采样比对 |
| 27 | 窗口顺序用 CGWindowList 补 SCShareableContent 的无序 | CaptureTarget.swift:43 | capture 手测 |
