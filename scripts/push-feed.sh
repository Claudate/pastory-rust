#!/usr/bin/env bash
# Mirror the just-released zip + metadata onto the pastejar-feed Cloudflare
# stack (docs/MAS_PLAN.md §1). The Worker never reads GitHub itself — this
# script is its only writer. Skips quietly when CF env is absent: the app's
# updater then keeps using the GitHub source alone (two-tier fallback).
#
# usage: scripts/push-feed.sh <version> <notes-file.md> [zip-path]
#   e.g. scripts/push-feed.sh 1.0.8 notes.md
#        scripts/push-feed.sh 1.0.8 /tmp/notes.md build/PasteJar-1.0.8-macos-universal.zip
#
# env (see cloudflare/README.md):
#   FEED_HOST CF_API_TOKEN CF_ACCOUNT_ID CF_KV_NAMESPACE_ID
#   CF_R2_BUCKET (default pastejar-releases) · MAS_STORE_URL (optional,
#   written into the feed doc's `mas.store_url` once the App Store listing exists)
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:?usage: push-feed.sh <version> <notes-file> [zip]}"
NOTES="${2:?usage: push-feed.sh <version> <notes-file> [zip]}"
ZIP="${3:-dist/PasteJar-$VERSION.zip}"
[ -f "$ZIP" ] || { echo "push-feed: $ZIP missing"; exit 1; }
[ -f "$NOTES" ] || { echo "push-feed: notes file $NOTES missing"; exit 1; }

if [ -z "${CF_API_TOKEN:-}" ] || [ -z "${CF_ACCOUNT_ID:-}" ] || [ -z "${CF_KV_NAMESPACE_ID:-}" ] || [ -z "${FEED_HOST:-}" ]; then
    echo "push-feed: CF_API_TOKEN/CF_ACCOUNT_ID/CF_KV_NAMESPACE_ID/FEED_HOST not set — skipping (feed mirrors are optional; clients fall back to GitHub)"
    exit 0
fi
if ! command -v npx >/dev/null 2>&1; then
    echo "push-feed: npx (Node) missing — cannot run wrangler; skipping. Install Node or push manually with wrangler (cloudflare/README.md)."
    exit 0
fi
export CLOUDFLARE_API_TOKEN="$CF_API_TOKEN" CLOUDFLARE_ACCOUNT_ID="$CF_ACCOUNT_ID"

ASSET="$(basename "$ZIP")"
SHA="$(shasum -a 256 "$ZIP" | cut -d' ' -f1)"
JSON="dist/release-feed.json"
python3 -I - "$JSON" "$ASSET" "$SHA" "v$VERSION" "$NOTES" "$FEED_HOST" "${MAS_STORE_URL:-}" <<'PY'
import json, sys
out, asset, sha, tag, notes, host, mas = sys.argv[1:8]
doc = {
    "tag_name": tag,
    "html_url": f"https://github.com/Claudate/pastory-rust/releases/tag/{tag}",
    "body": open(notes, encoding="utf-8").read(),
    "assets": [{
        "name": asset,
        "browser_download_url": f"https://{host}/files/{asset}",
        "sha256": sha,
    }],
}
if mas:
    doc["mas"] = {"store_url": mas}
open(out, "w", encoding="utf-8").write(json.dumps(doc, ensure_ascii=False))
print(f"push-feed: built {out} (tag {tag}, asset {asset})")
PY

# Push the artifact first: a `latest` doc pointing at a not-yet-uploaded zip
# would strand the client's download on a 404.
npx --yes wrangler r2 object put "${CF_R2_BUCKET:-pastejar-releases}/$ASSET" \
    --file "$ZIP" --content-type application/zip
npx --yes wrangler kv key put latest --path "$JSON" \
    --namespace-id "$CF_KV_NAMESPACE_ID"
echo "→ feed: latest=v$VERSION · https://$FEED_HOST/files/$ASSET"
