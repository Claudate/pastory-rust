# PasteJar — Mac App Store 提审物料

版本 1.0.7（build 8）· 团队 DIGITAL BANG INTELLIGENCE LLC（HA7KN8S95K）· bundle id `com.cici.snipclip`

本目录是「提审」这一环的全部输入：文字、截图、隐私标签、审核备注都放在这里，
好让上传这一步只做打包和传输，不再临时决定文案。截图不是设计稿，而是
`--selftest` 渲染真实面板后合成的桌面实景——审核看到的就是商品本身；
代价是面板的宽扁比例决定了构图只能「贴上去」而不是铺满，`compose.py` 里
按目标宽度等比缩放，不裁切、不拉伸。

> 所有内容都可直接粘进 App Store Connect。图片在 `screenshots/zh|en/`，
> 上传命令见仓库根的 `upload-mas.sh`。

---

## 1. 必填元数据

### 名称 / 副标题 / 关键词

| 字段 | 中文（zh-Hans） | English (U.S.) | 字符数 |
|---|---|---|---|
| 名称 | `PasteJar - 剪贴板历史` | `PasteJar - Clipboard History` | 19 / 30 |
| 副标题 | `复制过的，都能找回来` | `Everything you copied, remembered` | 9 / 31 |
| 关键词 | `剪贴板,粘贴,历史记录,截图,标注,录屏,OCR,取词,效率,菜单栏` | `clipboard,paste,history,screenshot,annotate,capture,OCR,menubar,productivity` | — |

关键词规则：逗号分隔、不含空格、总长 ≤ 100 字符。上表中文 29 字符、英文 78 字符，均合规。
不建议把「免费」「最好」这类词写进关键词（审核会当作营销承诺）。

### 分类

- 主分类 **Productivity**，次分类 **Utilities**
- 年龄分级：**4+**（无用户生成内容、无广告、无社交功能）

### 价格与地区

- 首发建议 **免费**（与直发版一致，避免已有用户迁移时产生预期落差），后续再定
- 地区：全地区；中国大陆区需自行确认是否需要软著（非本物料范围）

---

## 2. 描述（App Store Connect「描述」字段，纯文本，无 Markdown）

### 简体中文

```
PasteJar 给剪贴板装了一罐长期记忆。

复制过的文字、链接、图片、截图、录屏，不再在「下一次复制」时消失，而是排成一排，等你要用的时候随时取。

■ 剪贴板历史
· 单击任意卡片即为复制，直接粘进目标输入框
· 双击卡片直接贴进当前应用，全键盘用户可切换「双击 + 回车」
· 搜索覆盖文本、标题，以及图片里的文字（本地 OCR）
· Pin 起来的内容长期保留，不受自动清理影响
· 重要内容可以加标题，一眼认出「客户 A 地址」「开票信息」
· 文本与图片都支持二次编辑，改完替换原卡片留在历史里
· 微信、飞书、系统截图的图，只要进了剪贴板就有独立卡片

■ 截图与标注
· 快捷键框选截图，颜色按显示器自身色彩空间取，不改画面原色
· 矩形、椭圆、箭头、直线、画笔、文字、马赛克，七种低饱和配色
· 文字框拖动四边即可调整，文字随宽度自动换行、字号不变
· 本地 OCR 识别图上文字，识别结果可编辑、可复制
· 录屏支持 MP4 与 GIF

■ 两种形态，随场景切换
· 面板：屏幕底部的抽屉，键盘与搜索驱动
· 晾衣绳：屏幕顶部一整条，照片挂在绳上轻轻摆动，鼠标手势呼出
· 两种形态 × 多套皮肤自由组合，外观跟随系统浅色 / 深色

■ 隐私
· 没有账号，没有统计，没有广告
· 所有数据都在你自己的 Mac 上（应用沙盒内的 Application Support）
· 唯一的联网请求是每天一次的版本检查，不带任何标识，可在设置里关闭
· 密码管理器标记为隐藏的内容不会被记录

PasteJar 是一个安静的菜单栏小工具：它不抢焦点，不弹窗打扰，只在你按下快捷键的时候出现。
```

### English (U.S.)

```
PasteJar gives your clipboard a long-term memory.

Text, links, images, screenshots and screen recordings no longer vanish on the next copy. They line up in one place, ready when you need them.

■ CLIPBOARD HISTORY
· Click any card to copy it, then paste into any app
· Double-click a card to paste straight into the current app; full-keyboard users can switch to "double-click + Return"
· Search covers text, titles, and the text inside images (on-device OCR)
· Pin anything to keep it forever, regardless of your cleanup schedule
· Give important items a title, so "Client A address" is obvious at a glance
· Edit text and images in place; the edited version replaces the original card
· Screenshots from other apps (system, chat apps) get their own card too

■ SCREENSHOTS & ANNOTATION
· Capture a region with a shortcut; colours come from your display's own colour space
· Rectangle, ellipse, arrow, line, pen, text and mosaic, in seven muted colours
· Drag a text box edge to resize it; text reflows, the point size stays put
· On-device OCR pulls text out of a screenshot; the result is editable and copyable
· Record the screen as MP4 or GIF

■ TWO FORMS, ONE TOOL
· Panel: a drawer at the bottom of the screen, keyboard- and search-driven
· Clothesline: a full-width strip along the top edge, with photos swaying on a rope, revealed by a mouse gesture
· Any form with any skin; appearance follows system light and dark

■ PRIVACY
· No account, no analytics, no ads
· Everything stays on your Mac, inside the app sandbox
· The only network request is a daily version check, with no identifiers, and you can turn it off
· Items that password managers mark as concealed are never recorded

PasteJar is a quiet menu-bar utility: it never steals focus and never interrupts. It appears when you press the shortcut, and gets out of the way when you are done.
```

---

## 3. 隐私标签（App Privacy）

App Store Connect 的问卷答案：**Data Not Collected（不收集数据）**。

逐题依据：

| 问卷项 | 答案 | 依据（可复核） |
|---|---|---|
| 是否收集数据 | **否** | 全部数据写入应用沙盒内 `Application Support/Pastory/`，不离开本机 |
| 第三方分析 / 广告 SDK | 无 | `rust/Cargo.toml` 无第三方 UI/统计依赖，仅系统框架 |
| 崩溃上报 | 无 | 无 Crashlytics 之类集成 |
| 网络请求 | 每天一次版本检查 | `rust/src/App/updater.rs`；商店版 `mas` feature 下只读 feed，不含下载可执行代码的通路 |
| 标识符 | 无 | 请求不带 IDFA / IDFV / 安装 ID；UA 仅含品牌与版本号 |

隐私政策 URL 为必填项，若暂无独立站点，可指向仓库的 `README.md#隐私` 或单独一页；
**该 URL 不能为空，否则无法提交审核。**

---

## 4. 审核备注（App Review Information → Notes）

这段直接粘进「备注」框，中英并列（审核人多数读英文）：

```
PasteJar is a menu-bar clipboard manager plus screenshot annotation tool.
Everything is stored locally; there is no account and no analytics.

1) Screen Recording (first screenshot only)
   Requested on demand with the system prompt, never at launch. Used solely to
   capture the region the user selects. No continuous capture.

2) Accessibility (com.apple.security.accessibility)
   Used for one thing: synthesising a single ⌘V keystroke when the user
   double-clicks a clipboard card, so the item lands in the app they were
   typing in. Paste and PopClip ship in the App Store with the same
   entitlement for the same purpose. It is not used to read keystrokes,
   monitor other apps, or collect input. If this entitlement is a blocker,
   the feature degrades to "copy + show ⌘V hint" and the rest of the app is
   unaffected.

3) No self-update
   The store build (feature `mas`) has the download / verify / self-replace
   chain compiled out entirely, per Guideline 2.5.2. The daily version check
   is read-only and, when a newer version exists, only offers "Open App Store".

4) Update feed
   One HTTPS GET per day to our own feed endpoint, no identifiers, no body
   content, can be disabled in Settings. There is no other network traffic.

5) Sign in
   No account or sign-in is required. All features are available on first launch.

6) Difference from other clipboard tools
   Two independent interaction forms (bottom drawer / top "clothesline" strip),
   a skin system, and an annotation canvas with on-device OCR are built in
   rather than sold separately. Screenshots 1-5 show both forms and the
   annotation surface.

Contact: goodjackwork@gmail.com
```

---

## 5. 截图（`screenshots/zh/`、`screenshots/en/`）

| 文件 | 内容 | 说明 |
|---|---|---|
| `01-clipboard.png` | 剪贴板历史主面板 | 首个截图位，商店列表里直接展示 |
| `02-search.png` | 关键词搜索结果 | 展示本地搜索 + OCR 覆盖 |
| `03-settings.png` | 设置 · 外观（两种形态切换） | 证明形态差异，回应 4.2 |
| `04-annotate.png` | 截图标注画布 | 矩形/箭头/文字/马赛克工具条与调色 |
| `05-clothesline.png` | 晾衣绳形态 | 第二形态独立证据，回应 4.2 |

规格：**2880 × 1800**（16:10，Mac App Store 接受的最大尺寸之一），RGB，PNG。
面板像素全部来自 `--selftest` 自测渲染，仅做桌面背景合成与等比缩放，未重绘 UI。

已逐张校验（10/10 通过）：

```bash
python3 -c "
from PIL import Image; import glob
for p in sorted(glob.glob('store-assets/screenshots/*/*.png')):
    im = Image.open(p); print(p, im.size, im.mode, im.size == (2880, 1800))
"
```

重新生成：

```bash
cd rust && cargo build -r --features mas && cd ..
BIN=rust/target/release/pastory S=/tmp/pastory-mas-store
mkdir -p store-assets/raw
for L in zh en; do
  PASTORY_LANG=$L PASTORY_STORE=$S "$BIN" --selftest shelf      store-assets/raw/shelf-$L.png
  PASTORY_LANG=$L PASTORY_STORE=$S "$BIN" --selftest shelfsearch store-assets/raw/search-$L.png
  PASTORY_LANG=$L PASTORY_STORE=$S "$BIN" --selftest settings   store-assets/raw/settings-$L.png
  PASTORY_LANG=$L PASTORY_STORE=$S "$BIN" --selftest annotate   store-assets/raw/annotate-$L.png
  PASTORY_LANG=$L PASTORY_STORE=$S "$BIN" --selftest line       store-assets/raw/line-$L.png
done
python3 store-assets/compose.py
```

---

## 6. 应用图标

已生成：`store-assets/icon/appstore-1024.png` —— 1024×1024、**RGB 无 alpha**。

图标源：`public/pj-logo-icons/macos/icon_512x512@2x.png`（1024×1024、RGBA，四角是
烘好的圆角、`alpha=0`）。App Store Connect 既拒收带透明通道的图标，也**不接受**
把圆角烘进像素——那会和商店自己的裁切叠成双圆角、四角露白。所以做法是从圆角内
逐圈向外**扩散填充**纸纹把四角补满，圆角内的像素一个都不动：

- `appstore-1024-fullbleed.png` —— 母版，四角是延续的纸纹（提交用这张）
- `appstore-1024.png` —— 同一张压平成 RGB，兼容旧引用

复核（都该通过；四角是纸色，不是纯黑也不是白边）：

```bash
sips -g pixelWidth -g pixelHeight -g hasAlpha store-assets/icon/appstore-1024-fullbleed.png
# → 1024 / 1024 / hasAlpha: no
```

改图标时重新生成（源图换一行路径即可）：

```bash
python3 - <<'PY'
from PIL import Image
import numpy as np

src = np.asarray(Image.open("public/pj-logo-icons/macos/icon_512x512@2x.png").convert("RGBA"), dtype=np.float64) / 255.0
rgb, a = src[..., :3], src[..., 3:4]

def shift(arr, dy, dx, fill):
    """不环绕的平移：np.roll 会让纸纹从对边串过来。"""
    out = np.full_like(arr, fill)
    h, w = arr.shape[:2]
    ys, yd = (slice(dy, h), slice(0, h - dy)) if dy >= 0 else (slice(0, h + dy), slice(-dy, h))
    xs, xd = (slice(dx, w), slice(0, w - dx)) if dx >= 0 else (slice(0, w + dx), slice(-dx, w))
    out[yd, xd] = arr[ys, xs]
    return out

# 逐圈取四邻域已知像素的均值向外扩散，直到满幅（本项目约 130 轮）。
filled = rgb.copy()
valid = a[..., 0] > 0.5
while not valid.all():
    acc = np.zeros_like(filled)
    cnt = np.zeros((rgb.shape[0], rgb.shape[1], 1))
    for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
        sv = shift(valid, dy, dx, False)[..., None]
        acc += shift(filled, dy, dx, 0.0) * sv
        cnt += sv
    newly = (~valid) & (cnt[..., 0] > 0)
    filled[newly] = acc[newly] / cnt[newly]
    valid = valid | newly

out = rgb * a + filled * (1.0 - a)          # 圆角内保留原图，圆角外用延展的纸纹
full = Image.fromarray((np.clip(out, 0, 1) * 255 + 0.5).astype(np.uint8))
full.save("store-assets/icon/appstore-1024-fullbleed.png", "PNG")
full.convert("RGB").save("store-assets/icon/appstore-1024.png", "PNG")
PY
```

一个踩过的坑：拿 `np.roll` 连做 ~120 次五点平均当模糊用，扩散半径只有十几像素，
补不满圆角——四角会变成纯黑（`(0,0,0)`）。所以改成上面这种逐圈扩散，判断标准就是
四角 RGB 落在纸色附近（约 `245,240,231`）。

注意 `.icns` 本身仍保留透明圆角——那是给 app 自己用的，桌面 Dock 需要它。

---

## 7. 上传前的硬性阻塞项（截至 2026-10-22 核实）

这些不是物料，缺了 **Transporter 一定拒收**：

| # | 缺什么 | 现状（2026-10-22 实测） | 怎么做 |
|---|---|---|---|
| 1 | ~~3rd Party Mac Developer Installer 证书~~ **（误判，已撤销）** | 证书**在钥匙串里**，私钥也在。之前用 `find-identity -p codesigning` 探测才误报「缺失」——installer 证书的 EKU 不是 code signing，会被该 policy 过滤掉。真正卡住的是 `.pkg` 签名报 EPERM（会话级，见下） | 见下方「2026-10-22 复测修正」 |
| 2 | **App Store provisioning profile** | `~/Library/MobileDevice/Provisioning Profiles/` 里只有 `EasyAgent_MAS_Profile`，其 application-identifier 是 `HA7KN8S95K.com.clawpond.easyagent`——别的 app | Certificates, IDs & Profiles → Profiles → **+** → **Mac App Store** → 选 `com.cici.snipclip` 与 Apple Distribution 证书 → 下载 |
| 3 | **bundle id 是否已在门户登记** | 本机无法只读确认 | 门户 Identifiers 里必须有 `com.cici.snipclip`；没有就新建（Mac App Store 类型） |
| 4 | **App Store Connect 里的 app 记录** | 未见 | App Store Connect → My Apps → **+** → New App，选 Mac App Store，bundle id 选上面那个 |
| 5 | **上传凭据** | `~/.appstoreconnect/private_keys/` 不存在；无 Transporter 登录记录 | 见下方「凭据」一节，两种任选其一 |
| 6 | **版本号** | `CFBundleShortVersionString 1.0.7`、`CFBundleVersion 8` | 若 ASC 里已有 1.0.7 build 8，需要递增 `CFBundleVersion` |

`upload-mas.sh` 对这些项都是硬失败（不会默默出一个会被拒的 ad-hoc 包）。

### 2026-10-22 复测修正：#1 是误判，#2 是真阻塞

上一版把 installer 证书写成「钥匙串里没有」——**错了**。证书与私钥都在，
`security find-identity -v`（**不带** `-p codesigning`）能列出两条：

```
1) 38BD5B0B…  Apple Distribution: DIGITAL BANG INTELLIGENCE LLC (HA7KN8S95K)
2) 2E206BAD…  3rd Party Mac Developer Installer: DIGITAL BANG INTELLIGENCE LLC (HA7KN8S95K)
```

installer 证书有效期到 2027-04-17，私钥确实存在（把身份导出成 `.p12` 解析可见两个
证书各自带一把私钥）。误判只因探测命令带了 `-p codesigning`。`build-mas.sh` 与
`upload-mas.sh` 都已改成 `find-identity -v`，否则脚本会在证书明明存在时静默跳过
`productbuild`。

**但 `.pkg` 目前仍签不出来**，原因是执行会话限制，不是证书：

```
productsign: SignData failed: CSSM Exception: 100001 UNIX[Operation not permitted] (EPERM)
```

同一会话里连一条普通密码都写不进 login keychain：
`security add-generic-password` → `SecKeychainItemCreateFromContent: UNIX[Operation not permitted]`（exit 161），
`security import` 同样 EPERM。对照之下**读取已有私钥是通的**——
`codesign --sign "Apple Distribution: …"` 能成功。即「可读、不可写」的会话级限制。
重建 installer 私钥的 ACL 必须把身份导入 login keychain，而导入正是被 EPERM 拦下的那一步，
所以在当前会话里无解。

**解法（在正常 GUI 登录会话里做，不需要重新申请证书）**：

1. 钥匙串访问 → 登录 → 我的证书 → 找到 `3rd Party Mac Developer Installer: …`，
   展开其私钥 → 右键导出为 `.p12`。
2. 在同一会话里双击该 `.p12` 重新导入登录钥匙串；或右键私钥 → 显示简介 →
   访问控制 → 勾选「允许所有应用程序访问此项目」。
3. 重跑 `./upload-mas.sh`，确认不再出现 EPERM。

若在 Finder 双击打开的终端里仍 EPERM，说明 keychain 访问会话没建立：注销再登录一次
（或重启）后重试。

### 旁证：`security cms -D` 也受同一限制

`security cms -D -i x.mobileprovision` 在本机会报 `Operation not permitted`（早前记的「profile 损坏」
是误读）。读 bundle id 改用
`strings -a x.mobileprovision | grep -oE 'com\.[a-z0-9._-]+'` 仍可读出。实测本机仅有的 profile
属于 `com.clawpond.easyagent`，**不是** `com.cici.snipclip`。

### 凭据（二选一，`upload-mas.sh` 直接读环境变量）

**A. App Store Connect API Key（推荐，可完全脚本化）**
App Store Connect → Users and Access → Integrations → **App Store Connect API** → 生成密钥（**App Manager** 权限），下载 `AuthKey_XXXXXX.p8`，放到：

```bash
mkdir -p ~/.appstoreconnect/private_keys
mv ~/Downloads/AuthKey_XXXXXXXXXX.p8 ~/.appstoreconnect/private_keys/
```

记下 **Key ID** 与 **Issuer ID**，然后：

```bash
export ASC_KEY_ID=XXXXXXXXXX ASC_ISSUER_ID=xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
export PROVISION_PROFILE=~/Downloads/PasteJar_MAS.provisionprofile
./upload-mas.sh --upload
```

**B. Apple ID + App 专用密码**
appleid.apple.com → 登录与安全 → App 专用密码 → 生成。

```bash
export ASC_USERNAME=goodjackwork@gmail.com ASC_PASSWORD=xxxx-xxxx-xxxx-xxxx
export PROVISION_PROFILE=~/Downloads/PasteJar_MAS.provisionprofile
./upload-mas.sh --upload
```

密码经 `-p @env:ASC_PASSWORD` 传给 Transporter，不出现在进程命令行里。

> 不再推荐的第三种：先在 Transporter.app 图形界面登录一次复用会话——
> 会话会过期，且失败时只剩一句 401，脚本化排查不了。

> 账号：截图里的 Apple ID 是 `goodjackwork@gmail.com`，团队 `HA7KN8S95K`。
> 该账号必须是 App Store Connect 里 **Admin 或 App Manager**，否则上传会报 401/403。

---

## 2026-10-30 订正

上面第 7 节「上传前的硬性阻塞项」有两处已过时，以本节为准：

- **#1 installer 证书**：在。判定要用 `security find-identity -v`（**不带** `-p codesigning`），
  codesigning policy 会把 installer 证书滤掉，看起来像「没有」。
- **#2 profile**：`~/Library/MobileDevice/Provisioning Profiles/` 现在是**空目录**（EasyAgent 那份也已不在）。
  `.pkg` 签名在 2026-10-28 复测时已能出（EPERM 未复现），别再引用「签不出 pkg」的旧结论。
- **#3/#4/#5 现在可脚本化**：证书、App ID、ASC 记录、profile、上传都有脚本覆盖，
  不用再去门户点菜单。一条命令：`./MAS-上架.command`；说明见 [../docs/MAS_AUTO.md](../docs/MAS_AUTO.md)。
