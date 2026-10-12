# Mac App Store 上架与 Cloudflare 更新源改造计划

> 决策 + 分阶段任务清单。行为基准仍是 [DEVELOPMENT.md](DEVELOPMENT.md)；本文件只写「为了上架和国内网络可达性要改什么、按什么顺序」。
> 结论先行：**换皮不解决上架资格，沙盒化才是主体工作；联网不砍掉，而是把 GitHub 依赖换成 Cloudflare（feed 读 + R2 制品镜像 + 客户端回退 GitHub）。**

## 0. 已定决策

| 决策 | 内容 | 状态 |
|---|---|---|
| 双通道并存 | 直发版（Developer ID + zip，现状）与商店版（MAS 沙盒）同一仓库、cargo feature `mas` 分叉；直发管线不动 | ✔ |
| 联网策略 | 更新检查保留，主源改为自建 Cloudflare feed，GitHub API 降级为兜底源；**商店版只读 feed、绝不下载 zip**（Guideline 2.5.2 禁止下载可执行代码，更新走 App Store 本身） | ✔ |
| bundle id | 直发版永远 `com.cici.snipclip`（CLAUDE.md 硬规则）；商店版提交前必须换自有 id | 用户定：后期再换 |
| 品牌 | 商店版定名 **PasteJar**（2026-10-12 拍板）；sweep 见 commit「brand: rename to PasteJar」 |
| 联系面板 | X/微信/咖啡三格暂保留（按用户指示）；**提审前必须替换或经 mas cfg 移除** |
| 换皮分层 | L1 素材（图标/图钉/菜单符号）+ L2 色板与字体先行；L3 撕纸形状语言是否重做取决于商店版定位（官方商店版 vs 独立产品） | 待拍板 |
| 发布源仓库 | updater `REPO`、contact 面板 `REPO_URL`、`release.sh` 均还指向上游 `nothingbutcici/pastory`，而 CI/origin 已是 `Claudate/pastory-rust` —— 发布源已切 Claudate/pastory-rust（updater REPO、contact REPO_URL、release.sh，2026-10-12）；X/微信/咖啡链接随改名一并处置 | 完成 |

## 1. Cloudflare 架构

```
                 ┌───────────────────────────────────────────┐
 CI (tag 发布)   │  Cloudflare                               │
 dist.sh 之后 ──►│  wrangler kv put  RELEASES/latest ──┐     │      GET /v1/releases/latest
 release.json    │  wrangler r2 put  files/*.zip       │     │        ?channel=stable
 (jq 改写后的     │                                     ▼     │
  GitHub 响应)   │  Worker pastory-feed（只读 KV，无回源）─────┤
                 │    /v1/releases/latest  ← feed 元数据      │
                 │    /files/<name>.zip    ← 流式读 R2        │
                 └───────────────────┬───────────────────────┘
                                     │ HTTPS
              Pastory 客户端：主源 feed ──失败──► 兜底源 api.github.com（现有代码路径原样保留）
```

要点：

- **推送式发布，Worker 不回源**。CI 是唯一写入者：每次发布把改写过的 GitHub release JSON `wrangler kv key put` 进 KV，zip `wrangler r2 object put` 进 R2。Worker 纯读 KV/R2，不带 GitHub token、无外部依赖；feed 未命中就返回 404，客户端自然落到兜底源。
- **KV 的最终一致性（写后 ≤60s 全域可见）在此形态下无所谓**：单写入者、客户端本来就是 20h 节流 + 24h 轮询，不存在读己之写。
- **zip 由 Worker 从 R2 流式代理**（`/files/…`），与 feed 同域名单证书。R2 免出口流量：Class B（读）免费 10M/月，按 8 MB zip 算 10M 次 ≈ 80 TB 流量上限，远超免费档所需；若嫌绕，备选是给 R2 bucket 绑独立域名（不要用 `r2.dev` 公共 URL——那是防滥用限流的开发用途）。
- **国内可达性是这整件事的核心动机**：`api.github.com` 大陆直连不稳（updater 现有报错文案「可能需要代理」就是证据）。⚠️ 前提：**必须绑定自有域名**——`*.workers.dev` 在大陆长期不可靠（社区普遍反馈被 DNS 污染）。自有域名走 Cloudflare 边缘不保证大陆最优，但显著优于 GitHub；客户端的兜底链保证 feed 挂掉时行为不劣于今天。上线前用 TestFlight/直发版在大陆网络实测，这是 P1 验收项。
- **免费档预算核对**（2026-10 现价，上线前按 docs 复核）：Workers 免费 100,000 请求/天、每次 10ms CPU（流式代理不占 CPU，够）；KV 免费读 100,000/天、写 1,000/天（CI 每次发布写 1–2 键，够）；R2 免费 10 GB·月存储 + 1M Class A + 10M Class B，出口 $0。全部装得下当前体量；超限升级 $5/mo 付费档即可。文档、代码注释里不写死数字。

### 1.1 feed 元数据 schema

沿用 GitHub releases 响应子集 + 两个扩展字段，让客户端 `parse_release` 的既有取值逻辑零改动：

```json
{
  "tag_name": "v1.0.8",
  "html_url": "https://github.com/Claudate/pastory-rust/releases/tag/v1.0.8",
  "body": "中文说明\n---\nEnglish notes",
  "assets": [
    {
      "name": "Pastory-Rust-1.0.8-macos-universal.zip",
      "browser_download_url": "https://<feed-domain>/files/Pastory-Rust-1.0.8-macos-universal.zip",
      "sha256": "<hex>"
    }
  ],
  "mas": { "store_url": "https://apps.apple.com/app/id<数字ID>" }
}
```

- `browser_download_url` 由 CI 用 jq 改写指向 R2 代理路径；`sha256`、`mas` 是新增可选键——`parse_release` 按键取值，天然向前兼容。
- 真正的安全闸不变：下载后走现有 `verify_and_install` 的 codesign 校验链（anchor apple generic + 同 Team ID），来源域名只是白名单第一道。`sha256` 仅作 R2 完整性快检（P2 可选项），不引入自签哈希信任问题。
- 键：`latest`（stable 通道）。留 `?channel=` 查询参数在路由里但先只实现 stable——beta 通道以后只是多写一个键。
- 更新日志文案规范不变：中英两半 + `---` 分隔（`notes_for_display` 现有逻辑）。

## 2. 客户端改动（`rust/src/App/updater.rs`）

现状：`fetch_latest` 是「全 app 唯一网络请求」（GET api.github.com，15s 超时，UA 带版本号，无标识符）；`parse_release` 限定 zip 主机 ∈ {github.com, *.github.com, *.githubusercontent.com}；`offer → install → verify_and_install → relaunch` 完成自替换。

改动清单（直发版）：

1. `fetch_latest` 拆两级：先 feed（`https://<feed-domain>/v1/releases/latest`），非 200/网络失败/解析失败 → 再走现有 GitHub 请求。两路都产出同一个 `Release`。新增常量 `FEED_URL`（编译期可覆盖，方便 selftest 指向 fixture server）。
2. `parse_release` zip 主机白名单追加 feed 域名（含 `*.r2.dev`？——不加，见 1.1 备选说明；只加自有域名）。UA 串随 BRAND 常量走（见 §4 P0）。
3. `describe_error` 文案：「连不上 GitHub（可能需要代理）」改为不绑定单一来源的说法（新增 zh+en 对 + `App/Localization.swift` 表行规矩照 CLAUDE.md：新串 = 中文原面 + `.l` + 表行，`--selftest l10n` 查重）。
4. 自测（全部离线，符合仓库「updater 纯函数无网络可测」约定）：`--selftest updater` fixture 增加 feed-JSON 一例（含 `mas`/`sha256` 扩展键容错）、白名单判定表（feed 域名放行、任意第三方域名拒绝、http 拒绝）；`--selftest download` 主机过滤断言同步。

商店版（`mas` cfg，见 §3）：

- 编译期剥离：`install` / `verify_and_install` / `relaunch` / `can_self_install` / `update_progress` 窗口 / 下载 zip 的全部代码路径——二进制里不存在下载可执行体的通路，2.5.2 过审的硬条件。
- `offer` 只剩两键：「打开 App Store」（`NSWorkspace.openURL` feed 的 `mas.store_url`，`openURL` 现有先例同 contact_pane）与「稍后」。定时检查与「手动检查更新」保留（feed 只读），给的是「新版本公告 + 引导商店更新」，不是自装。
- 若后续审核纠缠，降级开关：feed 检查也整块 cfg 掉，商店版变纯离线（隐私标签更干净）。

## 3. 沙盒 / 签名 / 构建管线

- `Resources/Pastory-mas.entitlements`（新文件）：
  ```xml
  com.apple.security.app-sandbox          = true
  com.apple.security.network.client       = true   <!-- 只读 feed -->
  com.apple.security.accessibility        = true   <!-- 双击粘贴合成 ⌘V，先例 Paste/PopClip；审核重点，提交备注写明用途 -->
  ```
  直发版继续用空 dict 的 `Pastory.entitlements`，一行不改。
- **数据目录不用改**：`store.rs::default_root()` 走 `$HOME`，沙盒自动重定向进容器；TCC（屏幕录制、辅助功能）照常系统弹窗。
- **功能级风险点**（P4 真机清单，结论决定改不改）：
  - `panel.rs` 的 `addGlobalMonitorForEventsMatchingMask`（点外部收起 shelf）——沙盒内收不到其他 app 的鼠标事件，降级方案：失焦关闭 + Esc，或轻量轮询，实测后定。
  - Carbon `RegisterEventHotKey`、SCStream/SCScreenshotManager、Vision OCR、`NSPasteboard.changeCount` 轮询、`NSSavePanel/NSOpenPanel` 导出导入（powerbox）、`SMAppService.mainApp` 登录项、合成 ⌘V（辅助功能已授时）——预期沙盒内成立。
  - `com.apple.screenIsLocked/Unlocked` 分布式通知（monitor.rs 锁屏跳过）——部分通知沙盒内受限，实测。
  - 记住的导出目录跨启动失效（沙盒路径权限不随绝对路径持久）——改存 security-scoped bookmark，或退回「每次重选」。
- `build-mas.sh`（新）：`ARCHS="arm64 x86_64" cargo build --features mas` → 组装 bundle（同 build-rs.sh 清单）→ 嵌入 `embedded.provisionprofile`（App Distribution 证书 + MAS provisioning，Xcode/门户生成）→ `codesign --entitlements Pastory-mas.entitlements` → `productbuild` 出 pkg。不公证——上传 Transporter/App Store Connect 由苹果处理。
- `.github/workflows/`：新增 `workflow_dispatch` 手动 job 走 build-mas.sh + pkg 上传产物；现有 tag 驱动的直发 job 不动。发布后追一步 `wrangler kv key put` / `wrangler r2 object put`（token 进 Actions secret `CLOUDFLARE_API_TOKEN`）。
- **bundle id 换发时的连带**（后期执行，但 schema 已预留 `mas.store_url`）：新容器 = 历史/设置/TCC 全重来；迁移通道用现成的「导入」OpenPanel 选旧 `~/Library/Application Support/Pastory/`（powerbox 授权内复制可行），设置项与权限引导用户重点一遍——商店描述和首次启动引导要写清。

## 4. 分阶段清单

**P0 · 地基（0.5 天，先独立入库）**
- [ ] 把工作区未提交的全局改名 diff（Pastory → Pastory Rust，strings + selftest_m2/m4/m6 同步）单独成 commit。
- [ ] 抽 `BRAND` 常量（新 `rust/src/App/brand.rs` 或 theme.rs 顶部），收掉散落的字面量：`updater.rs::REPO`、User-Agent `Pastory-Rust/`、`exporter.rs` 文件名前缀 `Pastory {…}.png`（用户桌面可见，必换）、`selection_overlay.rs` TopBar `l_nonl8("Pastory")` + `text_width("Pastory")`、`delegate.rs` 状态项 accessibility description、`release.sh` 文案。
- [x] 发布源已切 Claudate/pastory-rust（updater::REPO、contact_pane REPO_URL、release.sh，含修复 release.sh 一直指错 dist/Pastory-*.zip 的陈旧路径；X_URL 与微信/咖啡处置并入改名轮）
- [ ] `cargo build` + 双语 shelf/annotate/ocrpanel 渲染自测过。

**P1 · Cloudflare feed + 客户端两级取源（1–2 天）**
- [x] `cloudflare/worker/`：`fetch` 路由 `/v1/releases/latest`、`/files/*`；`wrangler.jsonc`（KV namespace RELEASES、R2 bucket pastory-releases、自有域名路由）。Worker 不写 GitHub 回源逻辑。（代码已落地；**部署待 Cloudflare 账号 + 自有域名**）
- [x] `release.sh` 尾步 + CI 步骤调 `scripts/push-feed.sh` 生成 feed JSON（改写 zip URL + sha256）并推送 KV/R2；CF env 缺席时跳过、发布不阻塞。
- [x] `updater.rs` 两级 `fetch_latest`（`FEED_HOST` 编译期常量 `PASTEJAR_FEED_HOST`，未配置=纯 GitHub 现状）+ 白名单 `zip_host_allowed` + 报错文案中性化（更新源）+ l10n 锁步。
- [x] `--selftest updater` 增加 feed-schema fixture（扩展键容错 + 未知主机拒绝）；`download` 为真实联网项、按计划留到部署后验。
- [ ] 验收：浏览器打 feed 域名见 JSON；断网/错域名时 app 自动落 GitHub；大陆网络实测 feed 可达（workers.dev 对照组预期不通，证明绑域名必要）。

**P2 · R2 镜像细化（0.5 天，可与 P1 并）**
- [ ] `/files/*` 流式代理 + `Cache-Control`；zip 大小与免费档余量核对记录进 PR。
- [ ] （可选）`sha256` 快检进 `verify_and_install` 前置。

**P3 · mas feature + 商店变体（1–2 天）**
- [ ] `rust/Cargo.toml` 加 `mas = []`；updater/offer/安装链按 §2 分叉；`update_progress` 模块 cfg 隔离。
- [ ] `Pastory-mas.entitlements` + `build-mas.sh` + 手动 CI job + pkg 冒烟（本机装 TestFlight）。
- [ ] 验收：商店二进制 `nm`/strings 无 install/zip 通路；feed 检查 + 「打开 App Store」按钮可用。

**P4 · 沙盒真机回归（1–2 天，风险表见 §3）**
- [ ] 全局点击收起、锁屏通知、导出目录记忆三项出结论并落地（降级/bookmark/重选）。
- [ ] 双语 `--selftest shelf` 全面板目测（沙盒下渲染与非沙盒一致性）。
- [ ] 辅助功能授权流（合成 ⌘V 双击粘贴）+ 屏幕录制首启流在 pkg 安装形态下走通——MAS 的 TCC 条目随容器路径，重新验证一遍。

**P5 · 换皮与提交（独立轨道，定位拍板后启动）**
- [ ] L1 素材六件重制（MenuIcon 保持 template 剪影）；L2 色板/字体（theme.rs 常量 + `Resources/Fonts`，`fonts_resolve` 断言同步）。
- [ ] L3 形状语言：官方商店版→不动（品牌连续）；独立产品→四 path 函数重写 + 逐面板版式重调，预算 2–4 天。
- [ ] 联系面板商店版裁掉微信/咖啡二维码（`mas` cfg 或 feed 开关）；GitHub 链接可留。
- [ ] 提交材料：名称/副标题/截图（双语各一组，走 selftest 渲染管线）、隐私标签「不收集数据」、审核备注写明 accessibility entitlement 用途与 feed 性质。Guideline 4.2 抗辩点 = 沙盒化 + 自更新移除 + 更新源与发布主体独立 + （若走 L3）皮肤差异。
- [ ] bundle id 换发 + 数据迁移说明（§3 最后一条）。

## 5. 风险与备注

- **workers.dev 大陆可达性**：绑自有域名 + 客户端兜底 GitHub，已在架构里兜住；测出自有域名也不稳时，备选是国内对象存储镜像（超免费档，另议），不在本期。
- **`com.apple.security.accessibility` 审核**：Apple 会转专门审核（见 entitlements 文档），先例 PopClip/Paste/Magnet 均过；被拒的回退 = 商店版双击粘贴改「复制 + 提示 ⌘V」，功能降级但可上架。
- **KV/GitHub 双源分歧**：CI 推送顺序是先 GitHub Release 后 KV，feed 落后时客户端落兜底，行为一致；反向回滚（feed 已推、GitHub release 删）不要发生，release.sh 里保持同一提交点。
- **商店版隐私叙事**：全 app 唯一网络请求仍是「每天一次的版本检查」，无标识符无正文——dist 说明里「除每天一次检查更新外不联网」的措辞两通道都继续成立。
- **文档纪律**：本文件是计划，落定后把「行为约定」增补迁回 DEVELOPMENT.md（feed 协议、两级取源、mas 变体差异点进 §Rust 版差异点）。
