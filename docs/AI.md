# Pastory × AI 编程助手

> 给使用者：怎么让 Claude Code、Cursor、Codex 用上你已经复制过的东西。
> 给以后的自己：如果要做 MCP / CLI，接口该长什么样。
>
> **现状：** 没有 MCP、没有 URL scheme、没有面向用户的 CLI、没有 App Intents。零改代码的路径已经能用——库是普通 SQLite + 文件。

面向用户的产品介绍在 [README.md](../README.md) / [README.en.md](../README.en.md)。仓库里给编程助手改代码用的约定在 [CLAUDE.md](../CLAUDE.md) 和 [DEVELOPMENT.md](DEVELOPMENT.md)。

---

## 为什么值得结合

AI 编程助手的上下文是一次性的。你真正反复用的东西——能跑的 prompt、昨天的报错栈、一张 UI 参考、客户地址——几乎都先经过剪贴板。Pastory 已经在本机记下这些，截图还带 OCR。缺的只是一条助手读它的路。

同类产品已经在走：

- [Paste](https://pasteapp.io) 从 6.6 起内置 MCP（`npx add-mcp @pasteapp/mcp`）
- [Maccy](https://github.com/vlad-ds/maccy-clipboard-mcp) / [Maus](https://github.com/mnlt/maus-mcp) 有社区 MCP，读各自的本地 SQLite

Pastory 的存储比它们更透明：不加密、不沙盒、路径固定、schema 小。所以 **今天用 `sqlite3` 就能接**，不必等官方 MCP。

适合交给助手的，是你已经 Pin、起过标题、或刚刚复制的内容。不适合的是整库无过滤地倒进模型——里面可能有 token、cookie、聊天记录。

---

## 今天就能用（零改代码）

Pastory 运行时用 WAL 写索引；另一个进程用只读打开即可，不必退出 app。`busy_timeout` 是 3 秒。

```
~/Library/Application Support/Pastory/
  pastory.sqlite                 索引（WAL）
  pastory.sqlite-wal / -shm
  items/<id>.txt                 文本 / 链接正文
  items/<id>.rtf                 富文本（若有）
  items/<id>.heic | .png         图片（默认 HEIC；content_hash 永远是原 PNG 的哈希）
  items/<id>.json                文件条目：路径列表，不复制文件本体
  items/<id>.mp4 | .gif          录屏
  thumbs/<id>.heic               面板缩略图
  share/<id>/Rec yyyy-MM-dd …    录屏给剪贴板用的硬链接
```

`kind`：`text` / `url` / `image` / `files` / `video`。来源为导入的条目，`source_name` 存的是未翻译的 `"导入"`。

### 表结构

```sql
-- pastory.sqlite
CREATE TABLE items (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,          -- text | url | image | files | video
    created_at    REAL NOT NULL,          -- unix seconds
    source_bundle TEXT,
    source_name   TEXT,
    snippet       TEXT NOT NULL,          -- 文本前 400 字，或 "1280×720"
    ocr_text      TEXT,                   -- 图片的本地 OCR（中英）
    pinned        INTEGER NOT NULL,
    ext           TEXT NOT NULL,          -- txt png heic json mp4 gif
    has_rtf       INTEGER NOT NULL,
    pixel_w       INTEGER,
    pixel_h       INTEGER,
    byte_count    INTEGER NOT NULL,
    duration      REAL,                   -- 录屏秒数
    title         TEXT,                   -- 用户起的名，可搜
    content_hash  INTEGER NOT NULL,
    modified_at   REAL                    -- pin / 标题 / 编辑 / 再次复制
);
CREATE TABLE tombstones (
    id TEXT PRIMARY KEY,
    content_hash INTEGER NOT NULL,
    deleted_at   REAL NOT NULL
);
```

面板里的搜索不是 SQLite FTS，是内存子串，范围是 **全文 + OCR + 来源 app + 标题**。你在终端里搜，建议同样覆盖这四列，文本还要读 `items/<id>.txt`（`snippet` 只有前 400 字）。

### 查询例子

```bash
DB="$HOME/Library/Application Support/Pastory/pastory.sqlite"

# 最近 20 条（Pin 的排在前面更有用：先看 pinned）
sqlite3 -header -column "$DB" "
  SELECT datetime(created_at,'unixepoch','localtime') AS t,
         kind, printf('%.40s', coalesce(title, snippet)) AS what,
         source_name, pinned
  FROM items ORDER BY created_at DESC LIMIT 20;
"

# 按关键词搜标题 / 预览 / OCR / 来源（不含 txt 全文）
sqlite3 -header "$DB" "
  SELECT id, kind, title, snippet
  FROM items
  WHERE title    LIKE '%prompt%'
     OR snippet  LIKE '%prompt%'
     OR ocr_text LIKE '%prompt%'
     OR source_name LIKE '%prompt%'
  ORDER BY pinned DESC, created_at DESC
  LIMIT 30;
"

# 读一条文本的全文
ID=……
cat "$HOME/Library/Application Support/Pastory/items/$ID.txt"

# 最近一张截图的 OCR（报错、文档、UI 文案）
sqlite3 "$DB" "
  SELECT id, ocr_text FROM items
  WHERE kind='image' AND ocr_text IS NOT NULL AND ocr_text != ''
  ORDER BY created_at DESC LIMIT 1;
"

# 已 Pin 的文本 = 你的私人片段库
sqlite3 -header "$DB" "
  SELECT title, snippet, id FROM items
  WHERE pinned=1 AND kind IN ('text','url')
  ORDER BY modified_at DESC;
"
```

图片默认是 HEIC。给模型看之前转成 PNG（Pastory 自己导出也永远是 PNG）：

```bash
sips -s format png "$HOME/Library/Application Support/Pastory/items/$ID.heic" --out /tmp/pastory-$ID.png
```

### 在 Claude Code / Cursor 里怎么用

1. **临时一次：** 把上面某条命令的结果贴进对话，或让助手自己跑 `sqlite3`（只读）。
2. **反复用：** 在用户级技能 / 规则里写清路径和「先查 Pin，再查最近，不要 dump 全库」。Claude Code 把这段放进 `~/.claude/CLAUDE.md` 或一个 skill；Cursor 放进 User Rules。
3. **不要** 把 `pastory.sqlite` 当文本 `@` 进对话——那是二进制。也不要把整个 `items/` 目录丢进仓库。

一段可以直接交给助手的说明（复制到你的全局规则里）：

```
我的 macOS 剪贴板历史在 ~/Library/Application Support/Pastory/。
索引是 pastory.sqlite（WAL，可只读打开）。正文在 items/<id>.txt，
图片 OCR 在 items.ocr_text。需要旧 prompt、报错原文、截图里的字时，
先 SELECT pinned=1，再按 created_at 倒序搜 title/snippet/ocr_text；
命中后再读对应 items/<id>.txt。不要一次倒出全库。跳过看起来像
密钥、cookie、密码的内容。
```

---

## 四个已经成立的工作流

### 1. 能跑的 Prompt，下次还在

复制那一刻就进 Pastory。给它起标题（「翻译 prompt」「PR 描述」），Pin 住。下次在 Cursor / Claude Code 里说「把我 Pin 的翻译 prompt 拿来」，助手查库即可。不必再翻是哪个 session、第几十轮。

### 2. 报错截图 → 可搜索的字

Xcode / 终端 / 网页上的报错，⌥⌘S 截下来。Apple 本地 OCR（中英）写入 `ocr_text`，面板里能搜，助手也能搜。比「再手动划词复制一遍」少一步。

### 3. vibe coding 的视觉参考库

连续截参考站、竞品、自己跑起来的 UI。卡片横排，比较完 Pin 住喜欢的。助手需要看图时：用 `id` 找到 `items/<id>.heic`，`sips` 转 PNG 再读。导出到桌面也永远是全分辨率 PNG。

### 4. 定期整理成 Markdown 知识库

README 里那句「让 Agent 定期读数据库」可以具体成：

```bash
# 只整理 Pin 过的文本，写成一篇本地笔记（示例）
sqlite3 "$DB" "
  SELECT '# '||coalesce(title,'(untitled)')||char(10)||char(10)
       ||readfile(printf(char(36)||'HOME/Library/Application Support/Pastory/items/%s.txt', id))
  FROM items WHERE pinned=1 AND kind IN ('text','url')
  ORDER BY title;
"
```

`readfile()` 是 sqlite3 的扩展函数，有的构建没有。没有就对每个 `id` `cat` 一次。输出不要提交进这个 git 仓库。

---

## 如果以后要做接口（未实现）

按投入从小到大。**先做只读。** 写入会碰到去重、tombstone、密码管理器跳过、暂停开关，必须复用 `ClipStore` / `ClipboardMonitor` 的规则，不能自己插一行 SQL。

| 层级 | 做什么 | 谁受益 |
| --- | --- | --- |
| L0 | 本文：文档 + `sqlite3` 只读 | 现在 |
| L1 | 只读 MCP：`search` / `recent` / `get` / `pinned` | Claude Code、Cursor、Codex、Claude Desktop |
| L2 | 用户 CLI：`pastory search` `get` `copy` | 任何会 shell 的助手；也方便人 |
| L3 | 写入：`add --text` / `pin` / `title`（走运行中的 app，或复用 ClipStore） | 助手把结果存回历史，不必只靠系统剪贴板 |
| L4 | `pastory://search?q=` 、App Intents / Shortcuts | Alfred、系统快捷指令 |

### L1 MCP 草案

进程：独立小服务，**只读**打开 `pastory.sqlite`（`SQLITE_OPEN_READONLY`），需要正文时读 `items/<id>.txt`，需要图时解码成 PNG 再给。不要链进 Pastory.app 的 MainActor。

建议的 tools（名称稳定，宁可少）：

| tool | 作用 | 注意 |
| --- | --- | --- |
| `recent` | 最近 N 条元数据（默认 20，上限 50） | 返回 id、kind、title、snippet、source、time、pinned，**不**返回全文 |
| `search` | 关键词，走 title / snippet / ocr_text / source，可选再扫 txt | 默认排除未 Pin 的超长文本；命中再 `get` |
| `get` | 按 id 取全文，图片给 PNG（或路径） | 单条；图要设大小上限 |
| `pinned` | 只列出 Pin | 这是「片段库」 |
| `copy` | 把一条写回系统剪贴板 | 可选；等于人在面板里点了一下 |

不要做：dump 全库、按 bundle id 透视密码管理器、绕过「暂停记录」、读 tombstone 里的已删内容。

安全默认：

- 跳过 `snippet` / 正文像密钥的条目（`sk-`、`ghp_`、`-----BEGIN`、cookie 形状）。宁可漏，不要送。
- 不返回 `files` 条目指向的用户文件内容（那是路径引用，可能在家目录任意处）。
- 明文路径写进 MCP 配置即可，不要把数据库拷进云端 workspace。

Claude Code 用户级配置将来会是这种形状（现在还没有这个 server）：

```json
{
  "mcpServers": {
    "pastory": {
      "command": "pastory-mcp",
      "args": ["--readonly"]
    }
  }
}
```

Cursor 同理，写在 `~/.cursor/mcp.json`。

### L2 CLI 草案

```
pastory search "翻译 prompt"
pastory get <id>
pastory copy <id>
pastory pinned
```

实现上有两条路：

1. **旁路只读**（和 MCP 一样直接打开 SQLite）。简单，搜索与面板 100% 一致做不到（面板还搜 txt 全文，且有过滤胶囊）。
2. **问正在运行的 Pastory**（XPC / unix socket）。搜索一致，但 app 没开就失败。

建议 L1/L2 都走旁路只读；L3 写入再考虑 socket，避免第二个进程写 WAL。

### 不要做的

- 不要改 bundle id `com.cici.snipclip`。
- 不要在助手侧再做一套「剪贴板监听」——Pastory 已经在听，再听一份会把密码管理器跳过、锁屏、暂停搞丢。
- 不要把 HID 级按键注入当「粘贴 API」。双击粘贴只走 session 级 `CGEvent`，HID 曾经让 Command 整机卡住。
- 不要用 `--selftest` 当用户 API。它只在 `PASTORY_STORE` 指向临时目录时才允许写库，正常启动会忽略这些环境变量。

---

## 和「改 Pastory 源码的助手」的区别

| | 用 Pastory 当记忆 | 在本仓库里改代码 |
| --- | --- | --- |
| 读什么 | `~/Library/Application Support/Pastory/` | `Sources/`、`docs/DEVELOPMENT.md` |
| 给助手的说明书 | 本文 + 你全局规则里的那小段 | 仓库根目录 [CLAUDE.md](../CLAUDE.md) |
| 会不会碰到用户数据 | 会，只读、要过滤 | 自测必须 `PASTORY_STORE=/tmp/...`，碰不到真库 |

两件事可以同时成立：你用 Claude Code 写代码，同时让它在需要旧 prompt / 报错原文时查 Pastory。
