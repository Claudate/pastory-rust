# 铺速 PublishGo 素材模块缺陷修复提示词

这个文件记录的是 2026-10-29 PasteJar 上架草稿暴露出的两处铺速（listing-studio）缺陷，
以及可直接粘贴给处理该仓库的 AI 的修复提示词。放在 pastory 仓库里而不是铺速仓库，
是因为缺陷是在用铺速做 PasteJar 上架物料时被发现的：取证依据是这份草稿的 state.json
与两个仓库的源码，改动本体则属于铺速那边，所以这里只交「现象 + 根因 + 验收标准」。

取舍：没有直接修铺速源码，因为用户要的是「分析找出问题 + 给提示词」；提示词里刻意
不写具体补丁代码，只钉死判定口径与回归用例，避免把实现方案锁死在上游还没定的设计上。

> 用途：把下面「提示词正文」整段粘给处理 listing-studio 的 AI / 开发者。
> 取证时间：2026-10-29。取证方式：读 `listing-studio` 与 `listing-studio-mcp` 源码 + 直接读草稿 `data/state.json`。

---

## 提示词正文（从下一行开始复制）

我在用铺速 PublishGo 准备一个 Mac App Store 上架草稿（一个 macOS 应用 PasteJar），素材模块出了两类问题。请定位并修复软件本身的缺陷，不要只改我这份草稿的数据。

### 环境

- 桌面端源码：`/Users/dbi007/project/mac/listing-studio-migration/listing-studio`（当前 HEAD `8f99705`）
- MCP 源码：`/Users/dbi007/project/mac/listing-studio-migration/listing-studio-mcp`（当前 HEAD `c1b90da`）
- 草稿数据：`/Users/dbi007/project/test/8/listing-studio/data/state.json`
- 桌面端运行中，监听 `127.0.0.1:5178`，pid 16333
- 规格：`listing-studio-mcp/specs/appstore.json`

### 问题一：左侧栏「图片素材 1/1」与页面「已生成 2/10」互相矛盾，且错误地宣告「已完成、可以导出」

**现象**

- 左侧步骤栏「图片素材」显示 `1/1`，并打了绿勾（表示这一步完成）。
- 底部进度显示 `4/4 全部填完，可以导出了`。
- 但素材页里 Mac 2880×1800 卡片右上角写着「已生成 2/10」，App 图标卡片写着「已生成 1/1」。
- 这是一个 macOS 应用，它的必填素材应当只有「App 图标」+「Mac 2880×1800」两项，界面却按 1 项判定完成。

**已确认的根因（都实读代码核对过）**

1. 草稿里 `state.platforms.appstore.os` 是空字符串 `""`（其余三个平台 chrome / googleplay / windows 的 `os` 同样为空）。

2. 前端 `slotsForOs()`（`web/app.js:1532-1535`）的过滤规则是：

   ```js
   const os = (pf().os || "").trim();
   return (spec.assets || []).filter((a) => !a.os || (os && a.os === os));
   ```

   当 `os` 为空时，所有带 `os` 的槽位（`mac_2880` 属于 macos、`iphone_61` 属于 ios）**全部被滤掉**，只剩 `os: null` 的 `app_icon`。

3. 侧栏 `assetStats()`（`web/app.js:468-488`）遍历的就是 `slotsForOs()` 里的必填槽位，因此 `need = 1`、`done = 1`，得到 `1/1`。

4. 侧栏的绿色对勾来自 `renderRail()` 里的 `ready` 判定（`web/app.js:517-522`）：

   ```js
   const ready = slotsForOs(spec)
     .filter((a) => a.required)
     .every((a) => slotItems(a.id).length >= (a.minCount || 1));
   ```

   `os` 为空时它只检查 `app_icon`，于是返回 `true`，步骤被标成完成，底部随之显示「全部填完，可以导出了」。

5. 页面卡片上的「已生成 2/10」是**槽位自身的张数 / maxCount**（`mac_2880` 有 2 张、maxCount 为 10），与侧栏的「必填槽位够不够」是两套口径。两个数字放在同一屏上，用户无法判断到底还差什么。

**为什么 `os` 没有被自动填上**

`inferOs()`（`web/app.js:164-180`）只在「恰好一个系统下有图」时才推断系统：

```js
const withImages = variants.filter((v) =>
  spec.assets.some((a) => a.os === v && Array.isArray(assets[a.id]?.items) && assets[a.id].items.length)
);
if (withImages.length === 1) { pfState.os = withImages[0]; markDirty(); }
```

这份草稿里 `iphone_61`（ios）和 `mac_2880`（macos）**两边都有图**，`withImages.length === 2`，于是不推断、`os` 保持空。这条规则本身有它的道理（注释里写了「两边都有图说明在准备两套」），但由此产生的**未选系统状态，没有任何拦截或提示**，界面直接按「只剩图标」判定完成。

**要改的地方**

- 未选系统时，不要把素材步骤判成完成。`done` / `ready` 的计算至少在 `appstore` 这类存在多系统变体的平台上，要求 `os` 非空；或在 `os` 为空时明确显示「待选定系统」，并且底部进度不得出现「可以导出了」。
- 侧栏计数与页面计数必须同源同口径。建议侧栏在有多个系统变体且未选系统时，显示「需先选择 iOS / macOS」而不是 `1/1`；选定后再按生效槽位统计。
- 草稿同时存在 ios 与 macos 两套图时，给出明确冲突提示（例如「这是 Mac 应用，iPhone 6.1 英寸那张当前不用提交，建议删除或切换系统」），而不是静默地让自动推断失效。
- 加一条回归用例：`os = ""` + 只有 `app_icon` 有图 + `iphone_61` / `mac_2880` 均有图的草稿，断言不得显示「完成」或「可以导出」。

### 问题二：生成的 App 图标不对

**现象**

「App 图标」槽位生成的 1024×1024 图，是一张暖米色的图形（房子屋顶 + 一轮太阳 + 一家三口剪影），不是我的产品图标。

**已确认的根因**

1. 图标槽位和 iPhone 6.1 英寸槽位用的是**同一张源图**。`state.json` 里：

   - `assets.app_icon.items[0].src = "f37794c494d0bf5d996d1108af5b488756453d8a.png"`
   - `assets.iphone_61.items[0].src = "f37794c494d0bf5d996d1108af5b488756453d8a.png"`
   - `state.render.appstore.app_icon.assetId` 与 `iphone_61.assetId` 也都是这同一个 hash。

   源图实际文件是 `data/assets/uploads/f37794c494d0bf5d996d1108af5b488756453d8a.png`，就是那张房子图。

2. 渲染参数对图标不合适。`state.render.appstore.app_icon` 是：

   ```json
   { "mode": null, "pad": 0.1, "bg": "#ffffff", "radius": 0, "shadow": 0, "stroke": false }
   ```

   `mode` 为空时回落到规格的 `defaultMode: "contain"`（`specs/appstore.json` 中 `app_icon` 定义），配合 `pad: 0.1`，图形被缩到画布 90% 居中、四周填白底。App Store 图标应当是满幅的，不该有留白边。

3. `app_icon` 规格带 `"noDecoration": true`，注释写明「1024×1024 PNG，不能有透明通道，不要自己加圆角 —— iOS 与 macOS 都会自动裁切」。前端 `web/app.js:2063` 用 `if (!slot.noDecoration)` 来控制是否渲染装饰控件，这个分支是对的；但 `noDecoration` 只藏了 UI，**没有约束实际渲染参数**（`mode` / `pad` 仍可来自旧草稿或源图自带留白）。

4. 另有一处需要一并纠正：项目里已有的 `store-assets/icon/appstore-1024.png` 本身把圆角面板烘焙进了像素（浅蓝方底 + 圆角浅蓝面板 + 深棕手写 P），中间还夹了一圈更浅的边。这张图直接提交会与 Apple 的自动裁切叠加，产生双重圆角 / 边缘露色。正确的做法是提交**满幅方形、无圆角、无透明通道**的底图，由 Apple 裁切。

**要改的地方**

- `noDecoration: true` 的图标槽位，`mode` 应强制为 `cover`（或等价的满幅铺满），并忽略/禁用 `pad`；渲染时不得留白边。
- 源图与槽位要能看出「这张源图被复用到了哪些槽位」。同一张源图同时喂给图标槽位和 iPhone 截图槽位时，给出一条明确告警（一个 macOS 应用的图标和 iPhone 截图不该同源）。
- 图标槽位应优先从项目里自动识别真实应用图标（本项目的 `Resources/Logo.png` 512×512、`store-assets/icon/appstore-1024.png`），而不是要求用户手选一张任意图；识别不到时明确提示「未找到应用图标，请指定」。
- 加一条回归用例：图标槽位渲染后，四角像素不得与画布底色相同（即不得有留白边），且输出 PNG 无 alpha 通道。

### 验收标准

1. 打开该草稿（`os` 为空）：侧栏不得显示「1/1 + 绿勾」，底部不得显示「4/4 全部填完，可以导出了」；应明确提示需要先选 iOS / macOS。
2. 选定 macOS 后：侧栏计数应为「App 图标 + Mac 2880×1800」两项的完成情况，与页面口径一致。
3. App 图标槽位重新生成后：1024×1024、无 alpha、满幅无留白、圆角由 Apple 裁切，且与产品真实图标一致。
4. 同一张源图被用于不同系统的槽位时，界面给出可见告警。

### 不要做的事

- 不要通过直接改 `state.json` 让界面看起来正常；要改的是判定与渲染逻辑。
- 不要放宽规格校验来消除报错。
- 本机的 `listing-studio` 工作区有未提交改动（`crates/adapters/src/paths.rs`、`crates/app/src/main.rs`），动手前先确认这些改动是否属于你自己的范围，不要覆盖。

## 提示词正文结束
