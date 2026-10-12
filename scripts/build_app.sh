#!/bin/bash
set -euo pipefail
project_dir="$(cd "$(dirname "$0")/.." && pwd)"
app_dir="$project_dir/dist/随手找.app"
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
  source "$HOME/.cargo/env"
fi
cd "$project_dir"
cargo build --locked --release --bin miao-find
mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
cp target/release/miao-find "$app_dir/Contents/MacOS/MiaoFind"
cp packaging/Info.plist "$app_dir/Contents/Info.plist"
cp packaging/MiaoFind.icns "$app_dir/Contents/Resources/MiaoFind.icns"
codesign --force --deep --options runtime --timestamp=none --sign "${SIGN_IDENTITY:--}" "$app_dir"
codesign --verify --deep --strict "$app_dir"
printf '%s\n' "$app_dir"
