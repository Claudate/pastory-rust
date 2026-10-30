# PasteJar 上架一条命令（签名 · profile · 构建 · 上传）

> 这份文档对应 `MAS-上架.command` 与 `scripts/mas-*.{sh,rb}`。
> 目标：从「一台干净 Mac + 一个 Apple 开发者账号」走到「App Store Connect 里有可提交的构建」。
> 不装 Xcode、不用去门户里找菜单：证书、App ID、profile 都走 App Store Connect 接口建。

## TL;DR

```bash
git clone https://github.com/Claudate/pastory-rust.git pastory   # 私库先 gh auth login
cd pastory
./MAS-上架.command              # 出 dist/PasteJar-<版本>.pkg（可上传）
./MAS-上架.command --upload     # 再送 App Store Connect
```

人只需要：点一次命令行工具安装、输一次 macOS 密码（装 Homebrew）、给一次登录
（Apple ID + 2FA，或一个 ASC API Key）、钥匙串弹窗点「允许」。

先看缺什么：`./scripts/mas-doctor.sh`；只补环境：`./scripts/mas-bootstrap.sh`。

## 这台机器现在缺什么（2026-10-30 实测）

| 项 | 状态 |
|---|---|
| Command Line Tools / productbuild | 有 |
| Homebrew / cargo / fastlane 2.236.1 | 有（arm64 + x86_64 两个 Rust 目标都在） |
| Apple Distribution / Installer 证书 | 有（都有效到 2027-04-17） |
| provisioning profile | **没有** |
| ASC API Key（`.p8`） | **没有** |
| Apple ID 登录会话 | **没有** |

所以在这台机器上跑 `./MAS-上架.command`，它会：要一次登录凭据 → 建 profile → 构建 → 出 pkg。

## 三条路径

| 路径 | 什么时候用 | 人做什么 |
|---|---|---|
| **A · Apple ID + 2FA** | 不想建 API Key | 输密码 + 2FA（会话约一天）；`--apple-id you@example.com` 可指定账号 |
| **B · ASC API Key** | 想稳定、可重复（推荐） | 在 ASC 建一次密钥（1 分钟），导出 `ASC_KEY_ID` / `ASC_ISSUER_ID` |
| **C · 全手动** | Apple 两边接口都改坏了 | 门户点 App ID 与 profile（见文末附录），再跑 `build-mas.sh` |

建 API Key：<https://appstoreconnect.apple.com/access/integrations/api> → 「App Store Connect API」→ 生成密钥（权限 **App Manager**）→ 下载 `AuthKey_XXXXXXXXXX.p8` 放进：

```bash
mkdir -p ~/.appstoreconnect/private_keys
mv ~/Downloads/AuthKey_XXXXXXXXXX.p8 ~/.appstoreconnect/private_keys/
export ASC_KEY_ID=XXXXXXXXXX
export ASC_ISSUER_ID=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
```

## 脚本各自负责什么

| 文件 | 职责 |
|---|---|
| `scripts/mas-doctor.sh` | 只读诊断：工具链、两张证书与到期、每个 profile 绑的是谁、凭据、产物。`--probe` 附细节，`--json` 给脚本用 |
| `scripts/mas-bootstrap.sh` | 把干净 Mac 补到可用：CLT → Homebrew → rustup（两个架构目标）→ fastlane → 查 git/gh。每步先探测，可重复跑 |
| `scripts/mas-signing.rb` | 调 App Store Connect：查/建 App ID、建 ASC app 记录、建证书（本地生成私钥，直接导入钥匙串）、建 `MAC_APP_STORE` profile |
| `scripts/mas-auto.sh` | 主流程：环境 → 证书 → App ID → app 记录 → profile → 构建 → 自检 →（可选）上传 |
| `build-mas.sh` | 构建：默认 universal → 嵌 profile（**先校验绑的是本 app**）→ sandbox 签名 → `productbuild` 出 pkg |
| `upload-mas.sh` | 上传：`fastlane deliver` 为主，`iTMSTransporter` 兜底；缺 `PROVISION_PROFILE` 时直接调 doctor 说清原因 |
| `MAS-上架.command` | 双击入口，转调 `mas-auto.sh` |

## 每一步「成功长什么样」

| 步骤 | 成功输出 | 失败时最常见的三个原因 |
|---|---|---|
| doctor | 若干 `✓`，没有 `✗`，末尾 `READY` | 缺 CLT / 缺 fastlane / 没有 profile |
| 证书 | `✓ 已导入：Apple Distribution: …` | 角色不足（要 Admin）/ 证书到上限 / 钥匙串拒绝写入 |
| App ID | `✓ App ID 已存在`（或「已新建」） | bundle id 被别的团队占用 / 角色不足 |
| app 记录 | `✓ app 记录已存在` | ASC 没有建 app 的接口，脚本会打印表单并打开网页 |
| profile | `✓ profile 已写入 …`，且断言 `HA7KN8S95K.com.cici.snipclip` | App ID 与证书不属于同一团队 |
| 构建 | `→ dist/PasteJar-1.0.7.pkg (installer signed: …)` | 证书与 profile 不匹配 / 缺 Installer 证书 |
| 自检 | `✓ pkg 签名链完整`、`✓ 二进制 universal` | 见下方「签名链不完整」 |
| 上传 | `✓ 上传完成` | 凭据 401/403、build 号重复（409）、没有 app 记录 |

## 失败对照表

| 现象 | 含义 | 怎么做 |
|---|---|---|
| `xcode-select: error: tool 'xcode-select' requires...` / 没有 `productbuild` | 没装命令行工具 | 跑 `./scripts/mas-bootstrap.sh`，在弹窗点「安装」 |
| `curl: (7) Failed to connect ... raw.githubusercontent.com` | 网络（国内直连 GitHub 会超时） | 挂代理后重跑；rustup 可用 `--mirror cn` |
| `not allowed to create this certificate` / `403` | 账号角色不够 | ASC → 用户和访问 → 把该账号设为 Admin 或 App Manager |
| `You already have a current ... certificate` / 到上限 | 团队证书数量满 | 导入另一台机器导出的 `.p12`，或到门户吊销旧证书（会让那台机器的构建失效） |
| `证书建好了，但导入登录钥匙串失败` | 当前会话不能写钥匙串（IDE 终端常见） | 从 Finder 双击 `MAS-上架.command`，或钥匙串访问里改私钥 ACL |
| `这份 profile 绑的是「…」，不是 …` | 拿错 profile | 删掉它重跑 `./MAS-上架.command`；脚本会按 bundle id 重新建 |
| `这份 profile 带设备列表` | 下成了 Development / Ad Hoc | 要 Mac App Store 类型 |
| `pkg 签名链不完整` | 没用 Installer 身份签 | 跑 `./MAS-上架.command`（会建 Installer 证书） |
| `401` / `invalid credentials` | 凭据过期或 2FA 会话过期 | 重跑并重新登录；或换 API Key |
| `409` / build 号重复 | ASC 里已有同版本同 build | 把 `Resources/Info.plist` 的 `CFBundleVersion` 加一，重跑 |
| 上传后「处理中」很久 | Apple 端处理，正常 | 20 分钟后再看；失败会在 ASC 里给邮件/状态 |

## 上传之后（脚本到此为止）

1. ASC → 你的 App → **构建** → 选这次的版本。
2. 填 **审核联系人**（`review_first_name` / `review_last_name` / `review_phone`，草稿里是空的）。
3. 截图 5 张（`store-assets/screenshots/`，中英各一组 2880×1800）、隐私标签「不收集数据」、年龄分级。
4. 点「提交以供审核」。

文案从 listing-studio 的 App Store 平台导出，直接粘。

## 附录：全手动路径（路径 C）

只在脚本两条路都被 Apple 拒时用。

1. **App ID**：<https://developer.apple.com/account/resources/identifiers/list> → 左侧切到 **App IDs** → **+** → App IDs → Continue → App → Continue；
   Description `PasteJar`、Bundle ID 选 **Explicit** 填 `com.cici.snipclip`、Capabilities 一个都不勾 → Continue → Register。
2. **profile**：左侧 **Profiles** → **+** → Distribution 组里选 **Mac App Store** → 选 `com.cici.snipclip` → 选 `Apple Distribution: DIGITAL BANG INTELLIGENCE LLC (HA7KN8S95K)` → 名字 `PasteJar MAS` → Generate → Download。
3. 校验下载的文件：

   ```bash
   security cms -D -i ~/Downloads/*.provisionprofile > /tmp/p.plist 2>/dev/null \
     && /usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' /tmp/p.plist
   # 期望：HA7KN8S95K.com.cici.snipclip
   ```

4. 放到 `~/Library/MobileDevice/Provisioning Profiles/`，然后：

   ```bash
   PROVISION_PROFILE=~/Library/MobileDevice/Provisioning\ Profiles/<文件名> ./build-mas.sh
   ```

## 备注

- 脚本不替你**提交审核**，也不吊销任何证书：前者是发布动作，后者会让别的机器构建失效。
- `produce` 的 macOS 分支只认 `platform=osx`（写 `macos` 会建出 iOS 记录），脚本里是常量 + 断言。
- 会话与密钥的位置：Apple ID 会话在 `~/.fastlane/spaceship/`（约一天有效），API Key 私钥在 `~/.appstoreconnect/private_keys/`；脚本不复制、不上传这两个目录里的任何东西。

---

## 2026-10-30 修订（按独立审查逐条核对后改的）

- **凭据优先级**：ASC API Key（`ASC_KEY_ID` / `ASC_ISSUER_ID` + `~/.appstoreconnect/private_keys/AuthKey_<id>.p8`）
  是唯一不依赖 2FA 的路。Apple ID 那条改用 `ConnectAPI.login(..., use_portal: true)`——
  之前用 `use_portal: false` 时拿不到 provisioning 客户端，建 App ID / 证书 / profile 会全部抛
  `You need to instantiate this module with provisioning_request_client`（而且是在输完密码之后）。
  **API Key 仍建议优先**：它在 CI 和重跑时都稳定。
- **ASC 记录**：`App.create` 返回的是 HTTP 响应而不是模型，脚本现在建完会轮询复查；
  查不到先打印建 app 表单、打开网页、回车复验，最多三次，仍不行只 warn（pkg 照样出，上传前必须补）。
- **失败原因不再丢**：`mas-signing.rb` 的失败 JSON 同时写 stderr；`mas-auto.sh` 在 `die` 之前
  先把 `error` / `hint` 打出来——之前 `$(...)` 会吞掉 stdout，日志上只剩「看上面」而上面是空的。
- **手动下载了 profile**：`./MAS-上架.command --profile ~/Downloads/xxx.provisionprofile`
  直接用它，不再要求放到固定目录（脚本仍会校验它绑的是不是这个 bundle id、是不是 Mac App Store 类型）。
- **build 号冲突**：`--upload` 撞到 409 / `already been used` / `ITMS-4238` 时，自动把
  `Resources/Info.plist` 的 `CFBundleVersion` 加一、重出包、重传，最多两次，每次打印用的 build 号。
- **自检变成硬断言**：`build-mas.sh` 与 `mas-auto.sh` 都会在出包后核对
  pkg 签名链（到 Apple Root CA）、三份 sandbox entitlements、嵌入 profile 的 bundle id；
  任何一项不对就报出原因并退出，不再出现「已经打印可上传、上传时才被 Apple 拒」。
- **universal 目标名修正**：`arm64-apple-darwin` 不是合法 Rust target，已改成
  `aarch64-apple-darwin`（之前 universal 这条路一跑就 `could not find specification for target`）。
