# listing-studio 反馈：macOS App 上架物料链路的 6 个缺陷

实测环境：listing-studio-mcp `ab7ba16`，macOS 15.6 (arm64)，项目 PasteJar（原生 AppKit/Rust，Bundle ID `com.cici.snipclip`，Team `HA7KN8S95K`）。
每一条都有复现路径和当前行为，按「会让 agent 交出错误产物」的严重程度排序。

---

## 1. `appstore` 平台的素材槽位全是 iOS 机型，macOS 应用无处可放

**严重度：高（导致整块能力对 macOS 不可用）**

`get_spec("appstore")` / `list_assets("appstore")` 返回 7 个槽位：`app_icon`、`iphone_69`、`iphone_61`（必填）、`iphone_67`、`iphone_55`、`ipad_13`、`ipad_11`。

macOS App 在 App Store Connect 要的是 Mac 截图档：1280×800 / 1440×900 / 2560×1600 / 2880×1800（最低 1 张，最多 10 张），且没有「iPhone 6.1 必填」这个概念。

**复现**：对一个 `.app` 项目调用 `list_assets("appstore")`，`iphone_61` 被标为 `required: true` 且 `uploaded: false`，agent 会被引导去生成 iPhone 竖屏截图。

**影响**：agent 要么误报「还缺必填素材」，要么真去渲染一批 iPhone 尺寸的图给一个 Mac 应用。

**建议**：`appstore` 应区分 `platform: macos` / `ios`，或者至少增加 `mac_2880`（2880×1800）槽位并把 iOS 槽位移入 `ios` 子规格。

---

## 2. `inspect_project` 认不出 macOS 应用（.app / Info.plist）

**严重度：高**

```
inspect_project("/Users/dbi007/project/mac/pastory")
→ [MCP_ERROR] 没认出平台。
   看过的判据：manifest.json(manifest_version) / pubspec.yaml+ios /
               build.gradle(com.android.application) / Package.appxmanifest
```

一个结构完整的 macOS 应用：`Resources/Info.plist`（含 `CFBundleIdentifier`、`CFBundleShortVersionString`、`CFBundleVersion`、`LSMinimumSystemVersion`）、`build-mas.sh`、`Resources/PasteJar.entitlements`（`com.apple.security.app-sandbox`）、`.icns` 图标、签名脚本，全部齐备，但四条判据里没有一条覆盖 macOS。

**建议**：加判据 —— `*.entitlements` 且含 `com.apple.security.app-sandbox`、`*.xcodeproj`、`Info.plist` 且 `CFBundlePackageType=APPL`、`Package.swift` + macOS platform。抽出 `CFBundleIdentifier` / 版本号 / entitlements 里的权限键。

---

## 3. 无法从 macOS 的 `.icns` 或 entitlements 抽取可用事实

**严重度：中**

即使平台认出来了，macOS 的关键事实也不在它的抽取范围里：

- 图标是 `AppIcon.icns`（多尺寸容器），不是 1024×1024 PNG。要得到 App Store 要求的 1024×1024 无 alpha PNG 得从 `.icns` 里取最大 slice 再铺白底。
- 权限（Screen Recording / Accessibility）不是 Chrome 那种 `permissions` 数组，而是 entitlements 的布尔键。工具的 `permissions: null`（实测 `get_spec("appstore")` 返回 `permissions: null`）在 macOS 上等于没信息，但审核备注里必须写清每个 entitlement 的用途。

**建议**：`permissions` 对 appstore/macos 不适用是合理的，但应给出对应的「entitlements 说明」字段组，或至少在 `get_spec` 里说明 macOS 不需要按权限逐条填理由。

---

## 4. `validate` 不检查 URL 是否真实可达

**严重度：高（这次真的踩到了）**

草稿里 `support_url` / `privacy_url` 都指向 `https://github.com/Claudate/pastory`（该仓库**不存在**，`gh api` 返回 404），`validate` 只检查了「必填项是否为空」，返回 0 error。

App Store 审核对 Support URL / Privacy Policy URL 是硬性要求，指向 404 会直接拒审。

```
validate("appstore") → {"errors": 3, "warnings": 0}
# 3 个 error 全是 review_first_name / review_last_name / review_phone 为空
# 两个 404 的 URL 一个字都没提
```

**建议**：对 `type == "url"` 的字段做可达性探测（HEAD/GET，跟随重定向，超时可配），404/5xx 至少给 `warn`，并对 `privacy_url` 给 `error`。离线时降级为「未校验」而不是静默通过。

---

## 5. `export_listing` 的字符数校验与实际不符，且缺 macOS 化输出

**严重度：中**

导出的文本里有一段对 iOS 槽位的说明（「这套槽位是 iOS 的……可以忽略」）—— 这说明工具自己知道槽位不适用，但**这段话是靠人工写进导出模板的**，不是工具按平台判定的。对另一个 macOS 项目还会重复踩第 1 条。

另外字符数只按 `len(str)` 算，中文按 1 计。App Store 的 30 字符上限对中文实际是按字符计没错，但 mix 中英时容易和后台计数不一致（后台按 Unicode 标量，emoji 算 1 个字符但这里算 2）。

**建议**：导出时按平台渲染对应的素材说明段；字符数用 Unicode 字符计数并在接近上限时给 warn。

---

## 6. `draft_listing` 静默接受任意 key，只在写入时报 `rejected`

**严重度：低（但会浪费一轮）**

`draft_listing` 传了未知 key 时返回 `saved: true` + `rejected: [...]`，两者同时成立容易让调用方以为整体成功。实测传了合法 key 时表现正常，但这个返回值语义对 agent 不友好。

**建议**：有 `rejected` 非空时把 `saved` 改成 `partial: true`，或直接返回错误。

---

## 附：本次实测确认可用的部分

这些是对的，不用改：

- `get_spec` 的 `placeholder`、素材的 `altSizes` / `formats` / `maxBytes` 都正常返回。
- `listing_status` 的 `filledRequired` / `totalRequired` / `permissionsWithReason` 正常。
- `validate` 对平台名拼错（`chromee`）给出「可选：chrome / appstore / googleplay / windows」，提示清楚。
- `export_listing` 的排版可以直接粘进 App Store Connect。

---

## 最小修复优先级

1. **第 4 条**（URL 可达性）—— 唯一一条会让人真的交出错物料被判拒的。
2. **第 1、2 条**（macOS 支持）—— 决定这套工具对 macOS 项目有没有用。
3. 其余。
