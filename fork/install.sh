#!/bin/bash
# fork 版 herdr をビルドして ~/.local/bin/herdr に入れ、動いている herdr サーバーを
# live handoff で新しいバイナリへ引き継ぐ（ペインのプロセスは止まらない）。
# 引き継ぎ後、画面（クライアント）は終了するので、ターミナルで `herdr` を打ち直す。
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$HOME/.local/bin/herdr"
export ZIG="${ZIG:-$HOME/.local/share/zig/0.16.0/zig}"

cd "$REPO"
# 配布版（.github/workflows/fork-release.yml）と同じ版名を埋め込む。同じコミットなら
# 更新通知が出ず、より新しい配布版が出たときだけ `herdr update` の対象になる
export HERDR_BUILD_CHANNEL=fork
HERDR_BUILD_ID="$("$REPO/fork/build-id.sh")"
export HERDR_BUILD_ID
HERDR_BUILD_COMMIT="$(git rev-parse HEAD)"
export HERDR_BUILD_COMMIT
cargo build --release
NEW_VERSION="$("$REPO/target/release/herdr" --version | awk '{print $2}')"

# 実行中のファイルを直接上書きしないよう、別名で置いてから差し替える
install -m 755 "$REPO/target/release/herdr" "$DEST.new"

# macOS は「デスクトップ／ダウンロードへのアクセス許可」をバイナリの署名で覚える。
# ビルドしたままの署名は中身のハッシュそのものなので、入れ替えるたびに別アプリ扱いになり
# 許可を聞き直される。毎回同じ証明書・同じ名前で署名し直して、一度の許可を持ち越す。
# 証明書が無い環境では何もしない（HERDR_CODESIGN_IDENTITY で指定も可）
if [ "$(uname)" = "Darwin" ]; then
  SIGN_ID="${HERDR_CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/Apple Development|Developer ID Application/ {print $2; exit}')}"
  if [ -n "$SIGN_ID" ]; then
    codesign --force --sign "$SIGN_ID" --identifier dev.herdr.fork "$DEST.new" ||
      echo "codesign failed; macOS will ask for folder access again after this install" >&2
  else
    echo "codesign identity not found; macOS will ask for folder access again after each install" >&2
  fi
fi

mv -f "$DEST.new" "$DEST"
echo "installed $DEST ($NEW_VERSION, $(git rev-parse --short HEAD))"

# 子プロセス側の HERDR_* が付いていると、そのペインのサーバーに向くので外す
CURRENT_EXE="$(ps -ax -o pid=,args= | awk '$2 ~ /herdr$/ && $3 == "server" {print $2; exit}' || true)"
[ -n "$CURRENT_EXE" ] || { echo "running herdr server not found" >&2; exit 1; }
env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_SOCKET_PATH -u HERDR_BIN_PATH \
  "$CURRENT_EXE" server live-handoff --import-exe "$DEST" --expected-version "$NEW_VERSION"
