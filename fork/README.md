# このforkについて

[herdrdev/herdr](https://github.com/herdrdev/herdr) の個人用fork。`custom` ブランチが本体で、upstream の安定版タグ（現在 v0.9.5）を起点に改造を積んでいる。

## 改造点

| 内容 | 場所 |
| --- | --- |
| ctrl+click で文中の任意 `scheme://` リンク（`codex://` `file://` `vscode://` など）を開く。upstream は http/https だけ | `src/app/actions.rs`（`safe_web_url` / `url_prefix_len`） |
| ctrl+click でファイルパス（`src/a.rs:12`、`~/x.md`、絶対パスなど）を既定のアプリで開く。相対パスはそのペインで動いているプロセスの作業フォルダ基準。実在するものだけが対象 | `src/app/actions.rs`（`existing_path_at_byte`）、`src/app/api/plugins/mod.rs` |
| `herdr update` と更新通知を無効化（公式版で上書きされて改造が消えるのを防ぐ）。`HERDR_FORK_ALLOW_OFFICIAL_UPDATE=1` で解除 | `src/update.rs` |
| agentsパネルの並び替えに「status」を追加。確認待ち／実行中／監視中／完了の見出しで分ける（ヘッダ右端の並び替え表示をクリックして切り替え） | `src/agent_group.rs`、`src/client/shell/agent_sidebar.rs` |
| `herdr mcp`：エージェント用のMCP。`set_status`（question / waiting / done）で、ターンの終わり方をペインの `phase` に記録する | `src/cli/mcp.rs` |
| agentの行を右クリックすると、確認待ち／監視中／完了へ手動で移せる | `src/client/shell/context_menu.rs` |
| `phase` を保存し、live handoff や再起動の後も分類を残す | `src/persist/snapshot.rs`、`src/persist/restore.rs` |

改造箇所には `fork:` コメントを付けている。

## ビルドと入れ替え

```bash
fork/install.sh
```

ビルド → `~/.local/bin/herdr` へ配置 → 動いているサーバーを live handoff で新バイナリへ引き継ぐ（ペインのプロセスは止まらない）。

- 引き継ぎ後、画面（クライアント）は閉じるので、ターミナルで `herdr` を打ち直す。リンク判定はクライアント側にもあるため、開き直さないと改造が効かない
- ビルドには Zig 0.16.0 が必要。`~/.local/share/zig/0.16.0` に置いてある（Homebrew の zig は 0.17 で通らない）。別の場所なら `ZIG=/path/to/zig fork/install.sh`

## upstream への追従

```bash
git fetch upstream --tags
git rebase <新しいタグ>   # 例: v0.9.6
fork/install.sh
```

改造前後でテストを比べるときは `cargo test --release --no-fail-fast` を両方で流し、落ちたテスト名の差分を見る。v0.9.5 でも手元では約20件が実行環境しだいで不安定に落ちる。

## 公式版へ戻す

```bash
rm ~/.local/bin/herdr
brew unpin herdr && brew link herdr
```

その後 herdr サーバーを再起動する（`herdr server stop` → `herdr`）。ダウングレード方向の live handoff は試していない。

## このMacでの関連設定

- Homebrew 版 herdr は `brew unlink` + `brew pin` 済み（古いシェルから開くと Homebrew 版クライアントが起動してしまったため）
- 再起動時に claude の `--dangerously-skip-permissions` などを引き継ぐ hook: `~/.claude/hooks/herdr-claude-resume.py`（codex 用は `~/.codex/hooks/herdr-codex-resume.py`）
- herdr 内では `FORCE_HYPERLINK=1`（`~/.config/fish/conf.d/herdr-hyperlinks.fish`）。claude が Markdown リンクをクリック可能な形で出す
- `herdr mcp` は claude（`claude mcp add -s user herdr -- herdr mcp`）と codex（`~/.codex/config.toml` の `[mcp_servers.herdr]`）に登録している
- 申告忘れの差し戻し: `~/.claude/hooks/herdr-status-guard.py`（Stop hook）。そのセッションに `herdr mcp` が付いていて、今のターンで `set_status` を呼んでいないときだけ、1回だけ止めて申告させる。入れ子の `claude -p` は対象外
- fork 導入前のデータのバックアップ: `~/.config/herdr-backup-20261018`
