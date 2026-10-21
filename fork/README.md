# このforkについて

[herdrdev/herdr](https://github.com/herdrdev/herdr) の個人用fork。`custom` ブランチが本体で、upstream の安定版タグ（現在 v0.9.5）を起点に改造を積んでいる。

## 改造点

| 内容 | 場所 |
| --- | --- |
| ctrl+click で文中の任意 `scheme://` リンク（`codex://` `file://` `vscode://` など）を開く。upstream は http/https だけ | `src/app/actions.rs`（`safe_web_url` / `url_prefix_len`） |
| ctrl+click でファイルパス（`src/a.rs:12`、`~/x.md`、絶対パスなど）を既定のアプリで開く。相対パスはそのペインで動いているプロセスの作業フォルダ基準。実在するものだけが対象 | `src/app/actions.rs`（`existing_path_at_byte`）、`src/app/api/plugins/mod.rs` |
| `herdr update` と更新通知を無効化（公式版で上書きされて改造が消えるのを防ぐ）。`HERDR_FORK_ALLOW_OFFICIAL_UPDATE=1` で解除 | `src/update.rs` |
| agentsパネルの並び替えに「status」を追加。確認待ち／実行中／監視中／完了の見出しで分ける（ヘッダ右端の並び替え表示をクリックして切り替え） | `src/agent_group.rs`、`src/client/shell/agent_sidebar.rs` |
| `herdr mcp`：エージェント用のMCP。`set_status`（question / waiting / done）で、ターンの終わり方をペインの `phase`（時刻 `phase_at`、ひとこと `phase_note`）に記録する。waiting は「監視を設定済み（monitor_set=true）」と何を待つか（note）が必須。waiting の次の done は question に置き換え、監視結果をユーザーが見るまで閉じてよい扱いにしない。note は監視中・確認待ちの行の2行目に出す。done を自分で申告するのは、簡単なタスクで明らかに終わったときだけ。それ以外は返事の最後に「完了でいいですか？」と聞いて question（note「完了確認」）にし、ユーザーが同意したら done にする | `src/cli/mcp.rs` |
| agentの行を右クリックすると、確認待ち／監視中／完了へ手動で移せる | `src/client/shell/context_menu.rs` |
| エージェントのボード。`herdr mcp` の `update_board` で「あなたへ（判断してほしいこと／やってほしいこと）」「To-Do」「リンク」を一覧ごと置き換える。サイドバー下段に全エージェントのあなたへを出し、クリックでそのペインへ移る。どれかのエージェントがボードを持つと、ペインの右に幅34の欄が出て、操作中ペインのあなたへ・リンク・To-Do を折り返して表示する（ペイン幅が60列を切るなら出さない）。リンクはクリック（ctrl+クリックも可）で開く。エージェントには、あなた目線の言葉で書き、URL（起動したサーバー・公開した artifact・PR など）はできた時点で載せるよう説明している。`set_status` の返事で、残っているあなたへや、完了申告と食い違う To-Do を知らせる。live handoff・再起動後も残る | `src/board.rs`、`src/app/api/board.rs`（`pane.board.set`）、`src/client/shell/board_panel.rs`、`src/cli/mcp.rs` |
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
- 申告忘れの差し戻し: `~/.claude/hooks/herdr-status-guard.py`（claude と codex の両方の Stop hook から使う）。そのセッションに `herdr mcp` が付いていて、今のターンが始まった後に `set_status` が呼ばれていない（ペインの `phase_at` が古い）とき、または claude で waiting と申告したのに裏の処理も予約した確認も動いていないときだけ、1回だけ止めて申告し直させる。入れ子の `claude -p` と、MCP 登録前から動いているセッションは対象外
- codex の hook は信頼済みの印（`~/.codex/config.toml` の `trusted_hash`）が要る。`~/.codex/hooks.json` で hook の順番を変えると印が合わなくなり、codex が黙って hook を無視する
- fork 導入前のデータのバックアップ: `~/.config/herdr-backup-20261018`
