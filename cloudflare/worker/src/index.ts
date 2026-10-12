// pastory-feed — 只读的发布元数据 feed，为 PasteJar 客户端服务。
//
// 推送式：CI / release.sh 是唯一写入者（wrangler kv put + r2 object put），
// Worker 从不回源 GitHub，也不持有任何 GitHub token。feed 未发布时返回 404，
// 客户端按设计自动落到兜底源 api.github.com（docs/MAS_PLAN.md §1）。
//
// 路由：
//   GET /v1/releases/latest?channel=stable   KV:latest（JSON 直通；其他 channel 404）
//   GET /files/<name>.zip                    R2 流式代理（更新包下载）
//   GET /                                    纯文本 banner（人验证域名用）
//
// 键/对象的形状与 docs/MAS_PLAN.md §1.1 一致；sha256 与 mas 是扩展键，
// 客户端按键取值、未知键忽略。

interface Env {
  RELEASES: KVNamespace; // 元数据（latest 一个键）
  ASSETS: R2Bucket; // 发布包本体
}

const JSON_HEADERS = {
  "content-type": "application/json; charset=utf-8",
  "cache-control": "no-cache",
} as const;

function jsonError(message: string, status: number): Response {
  return new Response(JSON.stringify({ error: message }), {
    status,
    headers: JSON_HEADERS,
  });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (request.method !== "GET" && request.method !== "HEAD") {
      return jsonError("method not allowed", 405);
    }

    if (url.pathname === "/") {
      return new Response(
        "pastejar-feed: /v1/releases/latest (json) · /files/<zip> (release artifacts)\n",
        { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-cache" } },
      );
    }

    if (url.pathname === "/v1/releases/latest") {
      // 先只实现 stable；未知 channel 明确 404，客户端会落兜底源。
      const channel = url.searchParams.get("channel") ?? "stable";
      if (channel !== "stable") {
        return jsonError("unknown channel", 404);
      }
      const release = await env.RELEASES.get("latest", "json");
      if (!release) {
        return jsonError("no release published", 404);
      }
      return new Response(JSON.stringify(release), { headers: JSON_HEADERS });
    }

    const file = url.pathname.match(/^\/files\/(.+)$/);
    if (file) {
      const key = decodeURIComponent(file[1]);
      if (key.includes("..") || key.startsWith("/") || key.includes("\\")) {
        return jsonError("bad file name", 400);
      }
      const object = await env.ASSETS.get(key);
      if (!object) {
        return jsonError("not found", 404);
      }
      return new Response(object.body, {
        headers: {
          "content-type": object.httpMetadata?.contentType ?? "application/zip",
          "content-length": String(object.size),
          // zip 按版本命名、不可变，可放 CDN 缓存；feed JSON 走 no-cache。
          "cache-control": "public, max-age=86400",
          etag: object.httpEtag,
        },
      });
    }

    return jsonError("not found", 404);
  },
} satisfies ExportedHandler<Env>;
