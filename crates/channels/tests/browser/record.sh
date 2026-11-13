#!/usr/bin/env bash
# Record the persona demonstrations (tests/browser/demo.js): one captioned
# MP4 per persona, with an index.
#
#   crates/channels/tests/browser/record.sh OUT_DIR
#
# Needs Playwright with Chromium, and ffmpeg.
set -euo pipefail
out=${1:?the folder to write the videos to}
here=$(cd "$(dirname "$0")" && pwd)
raw=$(mktemp -d)
log=$(mktemp)
mkdir -p "$out"
cargo test -p ic-channels --test http serve_for_a_browser -- --ignored --nocapture >"$log" 2>&1 &
server=$!
trap 'pkill -f "http-.*serve_for_a_browser" || true; kill $server 2>/dev/null || true' EXIT
until grep -q "^policy" "$log"; do
  kill -0 $server 2>/dev/null || { cat "$log"; exit 1; }
  sleep 2
done
policy=$(grep "^policy" "$log" | head -1 | awk '{print $2}')
VIDEO="$raw" POLICY="$policy" node "$here/demo.js"
index="$out/index.md"
printf '# Persona demonstrations\n\nRecorded %s from the seeded portal. Each video is one persona doing one journey.\n\n| Video | Persona | What they do |\n| --- | --- | --- |\n' "$(date -u +%Y-%m-%d)" >"$index"
for clip in "$raw"/*/; do
  name=$(basename "$clip")
  webm=$(ls "$clip"*.webm | head -1)
  ffmpeg -v error -y -i "$webm" -c:v libx264 -pix_fmt yuv420p -preset veryfast -crf 26 -movflags +faststart "$out/$name.mp4"
  { read -r persona; read -r doing; } <"$clip/caption.txt"
  printf '| [%s.mp4](%s.mp4) | %s | %s |\n' "$name" "$name" "$persona" "$doing" >>"$index"
done
rm -rf "$raw" "$log"
echo "wrote $(ls "$out"/*.mp4 | wc -l) videos to $out"
