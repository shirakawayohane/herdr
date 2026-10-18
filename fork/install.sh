#!/bin/bash
# fork 版 herdr をビルドして ~/.local/bin/herdr に入れ、動いている herdr サーバーを
# live handoff で新しいバイナリへ引き継ぐ（ペインのプロセスは止まらない）。
# 引き継ぎ後、画面（クライアント）は終了するので、ターミナルで `herdr` を打ち直す。
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$HOME/.local/bin/herdr"
export ZIG="${ZIG:-$HOME/.local/share/zig/0.16.0/zig}"

cd "$REPO"
cargo build --release
NEW_VERSION="$("$REPO/target/release/herdr" --version | awk '{print $2}')"

# 実行中のファイルを直接上書きしないよう、別名で置いてから差し替える
install -m 755 "$REPO/target/release/herdr" "$DEST.new"
mv -f "$DEST.new" "$DEST"
echo "installed $DEST ($NEW_VERSION, $(git rev-parse --short HEAD))"

# 子プロセス側の HERDR_* が付いていると、そのペインのサーバーに向くので外す
CURRENT_EXE="$(ps -ax -o pid=,args= | awk '$2 ~ /herdr$/ && $3 == "server" {print $2; exit}' || true)"
[ -n "$CURRENT_EXE" ] || { echo "running herdr server not found" >&2; exit 1; }
env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_SOCKET_PATH -u HERDR_BIN_PATH \
  "$CURRENT_EXE" server live-handoff --import-exe "$DEST" --expected-version "$NEW_VERSION"
