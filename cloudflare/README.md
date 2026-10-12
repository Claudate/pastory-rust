# pastejar-feed（Cloudflare Worker）

PasteJar 更新检查的自建 feed：元数据在 KV（`latest` 一个键），发布包镜像在 R2（`/files/*.zip`）。
推送式发布 —— CI / `release.sh` 唯一写入，Worker 纯读、不回源 GitHub（设计见 [../docs/MAS_PLAN.md](../docs/MAS_PLAN.md) §1）。

## 一次性部署（需要 Cloudflare 账号 + 一个挂在 CF 上的自有域名）

```bash
cd cloudflare/worker
npm install --save-dev wrangler            # 或 pnpm dlx wrangler
npx wrangler login

# 1. 建 KV 命名空间，把返回的 id 填进 wrangler.jsonc 的 REPLACE_ME_KV_NAMESPACE_ID
npx wrangler kv namespace create RELEASES

# 2. 建 R2 桶（名字要和 jsonc 里 bucket_name、push-feed.sh 的 CF_R2_BUCKET 一致）
npx wrangler r2 bucket create pastejar-releases

# 3. 自有域名路由（Cloudflare 上已添加的站点）。⚠️ 不要用 *.workers.dev：
#    大陆不可靠，feed 的意义就是绕开这点（MAS_PLAN §5）。
#    在 dashboard（Workers & Pages → pastejar-feed → Domains & Routes）添加，
#    或把 routes 填进 wrangler.jsonc。

npx wrangler deploy
```

## 发布推送（release.sh / CI 调用 scripts/push-feed.sh）

推送脚本需要的环境（CI 放 Actions secrets）：

| 变量 | 例 | 用途 |
|---|---|---|
| `FEED_HOST` | `feed.example.com` | feed 域名（客户端编译期常量要同值，见 `updater.rs` 的 `FEED_HOST`） |
| `CF_API_TOKEN` | Token，权限 `Workers KV Storage:Edit` + `R2 Storage:Write` | wrangler 认证（`CLOUDFLARE_API_TOKEN`） |
| `CF_ACCOUNT_ID` | 账户 id | 同上（`CLOUDFLARE_ACCOUNT_ID`） |
| `CF_KV_NAMESPACE_ID` | 步骤 1 的 id | `wrangler kv key put --namespace-id` |
| `CF_R2_BUCKET` | `pastejar-releases` | `wrangler r2 object put --bucket` |

任一未设置时 `push-feed.sh` 打印提示并跳过 —— feed 缺席不影响发布（客户端自动落 GitHub 兜底源）。

## feed 元数据 schema（KV 键 `latest`）

GitHub releases 响应子集 + 两个扩展键，客户端 `parse_release` 按键取值：

```json
{
  "tag_name": "v1.0.8",
  "html_url": "https://github.com/Claudate/pastory-rust/releases/tag/v1.0.8",
  "body": "中文说明\n---\nEnglish notes",
  "assets": [{ "name": "PasteJar-1.0.8.zip",
               "browser_download_url": "https://<FEED_HOST>/files/PasteJar-1.0.8.zip",
               "sha256": "<hex>" }],
  "mas": { "store_url": "macrod://… MAS 上架后填 App Store 链接" }
}
```

## 验证

- `curl https://<FEED_HOST>/v1/releases/latest` → JSON
- `curl -I https://<FEED_HOST>/files/PasteJar-<ver>.zip` → 200、`content-length` 对得上
- 大陆网络实测 feed 可达（MAS_PLAN P1 验收项），对照 `api.github.com` 通不通。
- 免费档预算（上线前按 CF docs 复核，勿写死在代码里）：Workers 100k 请求/天、KV 读 100k/写 1k、R2 10 GB + 免出口流量。
