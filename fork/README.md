# このforkについて

[herdrdev/herdr](https://github.com/herdrdev/herdr) の個人用fork。`custom` ブランチが本体で、upstream の安定版タグ（現在 v0.9.5）を起点に改造を積んでいる。

## 改造点

| 内容 | 場所 |
| --- | --- |
| ctrl+click で文中の任意 `scheme://` リンク（`codex://` `file://` `vscode://` など）を開く。upstream は http/https だけ | `src/app/actions.rs`（`safe_web_url` / `url_prefix_len`） |
| ctrl+click でファイルパス（`src/a.rs:12`、`~/x.md`、絶対パスなど）を既定のアプリで開く。相対パスはそのペインで動いているプロセスの作業フォルダ基準。実在するものだけが対象 | `src/app/actions.rs`（`existing_path_at_byte`）、`src/app/api/plugins/mod.rs` |
| 更新は改造版の配布物からだけ受け取る（公式版で上書きされて改造が消えるのを防ぐ）。`custom` に push すると GitHub Actions が macOS・Linux 用の herdr と更新情報 `fork.json` をリリース `fork-<版>` として公開し、古いリリースは新しい10件を残して消す。herdr は動いている間30分ごとにそれを確認し、手元より新しい版があれば通知する。`herdr update` で入れ替わる。版名は `0.9.5-fork.<コミットのUTC日時>-<コミット先頭7桁>` で、手元で `fork/install.sh` したものも同じ名前になる。`HERDR_FORK_ALLOW_OFFICIAL_UPDATE=1` のときだけ公式版を見る | `src/update.rs`（`release_info_from_fork_manifest`）、`.github/workflows/fork-release.yml`、`fork/build-id.sh`、`fork/install.sh` |
| agentsパネルの並び替えに「status」を追加。確認待ち／実行中／監視中／完了の見出しで分ける（ヘッダ右端の並び替え表示をクリックして切り替え） | `src/agent_group.rs`、`src/client/shell/agent_sidebar.rs` |
| `herdr mcp`：エージェント用のMCP。`set_status`（question / waiting / done）で、ターンの終わり方をペインの `phase`（時刻 `phase_at`、ひとこと `phase_note`）に記録する。waiting は「監視を設定済み（monitor_set=true）」と何を待つか（note）が必須。waiting の次の done は question に置き換え、監視結果をユーザーが見るまで閉じてよい扱いにしない。note は監視中・確認待ちの行の2行目に出す。done を自分で申告するのは、簡単なタスクで明らかに終わったときだけ。それ以外は返事の最後に「完了でいいですか？」と聞いて question（note「完了確認」）にし、ユーザーが同意したら done にする | `src/cli/mcp.rs` |
| agentの行を右クリックすると、確認待ち／監視中／完了へ手動で移せる | `src/client/shell/context_menu.rs` |
| エージェントのボード。`herdr mcp` の `update_board` で「あなたへ（判断してほしいこと／やってほしいこと）」「To-Do」「リンク」を一覧ごと置き換える。サイドバー下段に全エージェントのあなたへを出し、クリックでそのペインへ移る。ペインの右に幅34の欄を常に確保し（ボードがなくても場所を取る。エージェントが1つもいないときだけ出さない。ボードの有無や画面の開き直しでペイン幅が一瞬変わり、Claude Code の表示が崩れたため）、操作中ペインのあなたへ・リンク・To-Do を折り返して表示する（ペイン幅が60列を切るなら出さない）。リンクはクリック（ctrl+クリックも可）で開く。エージェントには、あなた目線の言葉で書き、URL（起動したサーバー・公開した artifact・PR など）はできた時点で載せるよう説明している。`set_status` の返事で、残っているあなたへや、完了申告と食い違う To-Do を知らせる。live handoff・再起動後も残る | `src/board.rs`、`src/app/api/board.rs`（`pane.board.set`）、`src/client/shell/board_panel.rs`、`src/cli/mcp.rs` |
| 質問シート。ボードの1行では答えるのに情報が足りず聞き直しになるため、`update_board` のあなたへ（asks）に `detail`（Markdown。背景・分かった事実・選択肢ごとの結果・おすすめ・指定がないときの動き）と `options`（ワンクリックで選べる回答）を持たせ、herdr サーバーがペインごとのページ `http://127.0.0.1:<ポート>/<秘密の文字列>/<ペインID>` として配る。ボードのあなたへの下に「質問シートで答える」リンクが出る（`update_board` の返事にも URL が入る。`detail` のある判断の質問があるときだけ、エージェントは返信の最後に「回答は質問シートから」と用途を添えて URL を書く。作業のお願いだけのときや結果報告だけの返信には書かせない）。ページは3秒ごとに最新のボードを読み、書きかけはブラウザに保存される。「回答を送信」（⌘/ctrl+Enter）で、回答をそのペインのエージェントへ次の入力として送る（`agent.prompt`。エージェントと認識されていないペインには文字入力として送る）。許可待ちなどで送れないときは理由を出し、「コピー」で貼り付けられる。一目で答えられる簡単な質問には `detail` を書かせず、返信でそのまま聞かせる（シートの URL も書かせない）。`detail` のない ask は `update_board` の返事で名指しし、背景が要る質問なら書き足すよう促す。`detail`・`options` は端末の画面には送らない。ポートと秘密の文字列は `~/.config/herdr/sheet.json`（本人だけ読める）に保存し、live handoff・再起動後も同じ URL のまま。待ち受けは 127.0.0.1 だけで、秘密の文字列・Host ヘッダ・同一オリジンの JSON POST を確かめる。ssh 先で動かしている herdr のシートは、手元のブラウザからはそのままでは開けない | `src/sheet.rs`、`src/sheet/page.html`、`src/board.rs`、`src/cli/mcp.rs`、`src/client/shell/board_panel.rs` |
| claude の裏の会話（`/fork`・`claude --bg`）でも `herdr mcp` が正しいペインに書く。裏の会話は claude の常駐プロセスの中で動き、`HERDR_PANE_ID` が常駐プロセスを起動したペインのもの（無関係なペイン）になるため、`CLAUDE_JOB_DIR` があるときはそのペイン id を使わず、`claude attach <ジョブID>` を動かしているペインを呼び出しのたびに探して書く（前回見つけたペインを先に確かめる）。どこにも表示されていなければ何も記録しない。`set_status` と `update_board` の両方に効く | `src/cli/mcp.rs`（`PaneSource`） |
| 長いパスやURLのコピーで、アプリ（Claude Code など）が自分で入れた改行と続きの行の字下げを取り除いて1行に戻す。前の行が右端近く（8列以内）まで埋まって空白以外で終わり、次の行が字下げされていて、改行をまたぐ語に `/` か `\` を含むときだけつなぐ。Codex は長い語を `-` の直後でしか折らず右端から離れて終わることがあるので、`-` で終わる行は「次の行の最初の一区切り（空白まで、または最初の `-` まで）が残りの幅に入らなかった」ときも折り返しとみなす | `src/selection.rs`（`rejoin_app_wrapped_lines`）、`src/app/api/panes.rs`（`pane_selection_text`） |
| 複数行をコピーしたとき、全行に共通する行頭の空白（Claude Code の返答の2桁、ツール出力の5桁など）を取り除く。行どうしの相対的な字下げ（コードブロックなど）は残す。1行だけのコピーは対象外（ダブルクリックの単語選択が同じ読み取りを使うため） | `src/selection.rs`（`remove_common_indent`）、`src/app/api/panes.rs`（`pane_selection_text`） |
| ctrl+click とホバー下線も、アプリが自分で折り返した長いパス・URL（右端近くで切れ、次の行が字下げから続くもの。Codex の `-` での折り返しも上と同じ条件で含む）を1つのリンクとして扱う。つないだ結果に `/` か `\` を含むときだけ | `crates/ghostty-vt/src/lib.rs`（`extend_link_across_app_wraps`） |
| ctrl+click でのパス・URLの判定で、全角のかっこや句読点（`（）「」、。：？` など）を区切りとして扱う。日本語の文中で `プロンプト（/tmp/a.txt）は、` のように空白なしで挟まれていても開ける。URLは double-click の選択範囲も同じ所で止まる | `src/app/actions.rs`（`is_cjk_link_boundary`） |
| `phase` を保存し、live handoff や再起動の後も分類を残す | `src/persist/snapshot.rs`、`src/persist/restore.rs` |

改造箇所には `fork:` コメントを付けている。

## ビルドと入れ替え

```bash
fork/install.sh
```

ビルド → `~/.local/bin/herdr` へ配置 → 動いているサーバーを live handoff で新バイナリへ引き継ぐ（ペインのプロセスは止まらない）。

- 引き継ぎ後、画面（クライアント）は閉じるので、ターミナルで `herdr` を打ち直す。リンク判定はクライアント側にもあるため、開き直さないと改造が効かない
- macOS では入れ替えのたびに、毎回同じ証明書（キーチェーンの Apple Development か Developer ID。`HERDR_CODESIGN_IDENTITY` で指定可）と同じ名前 `dev.herdr.fork` で署名し直す。`fork/install.sh` と `herdr update`（`src/update.rs` の `sign_with_local_identity`）の両方で行う。こうしないと入れ替えのたびに別アプリ扱いになり、デスクトップ／ダウンロード等へのアクセス許可を毎回聞き直される。証明書が無い・期限切れのときは署名せずに入れ替えだけ行う（その回は聞き直される）
- ビルドには Zig 0.16.0 が必要。`~/.local/share/zig/0.16.0` に置いてある（Homebrew の zig は 0.17 で通らない）。別の場所なら `ZIG=/path/to/zig fork/install.sh`

## upstream への追従

`custom` は配布元で、使っている人の herdr がここから自動更新される。履歴を書き換えると配布と食い違うので、rebase はせず merge で取り込む。

```bash
git fetch upstream --tags
git merge upstream/master        # 安定版だけにしたいときは新しいタグ（例: v0.9.6）
cargo test --release --no-fail-fast
fork/install.sh                  # 手元で動作を確かめてから
git push origin custom           # これで配布版が作られ、全員に更新の通知が出る
```

- よくぶつかる所：本家の API 定義は `src/api/schema.rs` の `define_operations!` に1行ずつ並ぶ。改造の `pane.board.set` もそこにあり、`pane.report_metadata` は右クリックで状態を変えるため client shell からも受け付ける（行末が `true, true`）
- `src/api/schema/operations.rs` のテストが「画面が変わる命令」の一覧を持っているので、改造で命令を足したらそこにも足す。API の schema は `HERDR_UPDATE_API_SCHEMA=1 cargo test --release --bin herdr -- generated_protocol_schema_artifact_is_current` で作り直す
- テストは手元の設定しだいで落ちるものが十数件ある。取り込み前のコミットでも同じテストを流し、落ちたテスト名の差分だけを見る。並列実行のときだけ落ちるものは `--test-threads=1` で単独に流して確かめる

## 配布と自動更新

- `custom` に push すると `.github/workflows/fork-release.yml` が動き、10〜20分ほどでリリースが出る（`.md` と `docs/` だけの変更では動かない）
- 改造版を使っている人の herdr は30分ごとに最新リリースの `fork.json` を確認する。壊れた版を push すると全員に通知が届くので、push 前に手元で確かめる
- 新しく入れる人はリリースから自分の環境のファイルを `~/.local/bin/herdr` に置くだけでよい（ビルド不要）

## 公式版へ戻す

```bash
rm ~/.local/bin/herdr
brew unpin herdr && brew link herdr
```

その後 herdr サーバーを再起動する（`herdr server stop` → `herdr`）。ダウングレード方向の live handoff は試していない。

## このMacでの関連設定

- Homebrew 版 herdr は `brew unlink` + `brew pin` 済み（古いシェルから開くと Homebrew 版クライアントが起動してしまったため）
- 再起動時に claude の `--dangerously-skip-permissions` などを引き継ぐ hook: `~/.claude/hooks/herdr-claude-resume.py`（codex 用は `~/.codex/hooks/herdr-codex-resume.py`）
- claude で `/branch` か `/fork` をすると「左＝元の会話、右＝分岐」に並べる hook: `~/.claude/hooks/herdr-claude-branch-split.py`（SessionStart の source `fork` と SessionEnd の reason `resume` で動く）。`/branch` は同じ画面を分岐側に置き換えるので、分岐の記録（`forkedFrom`）にある元の会話を右に `claude --resume` で開いてから左右を入れ替える。直前に同じペインで元の会話が終わったときだけ動くので、`--fork-session` は対象外。権限フラグは `herdr-claude-resume.py` の処理で引き継ぐ。`/fork` は元の会話がそのまま残り、分岐は claude の常駐プロセスで裏の会話として動く（その通知に付くペイン番号は当てにならない）。裏の会話の `~/.claude/jobs/<短いID>/state.json` の `forkParentSessionId` から元の会話を動かしているペインを herdr に問い合わせ、その右に `claude attach <短いID>` を開く
- herdr 内では `FORCE_HYPERLINK=1`（`~/.config/fish/conf.d/herdr-hyperlinks.fish`）。claude が Markdown リンクをクリック可能な形で出す
- `herdr mcp` は claude（`claude mcp add -s user herdr -- herdr mcp`）と codex（`~/.codex/config.toml` の `[mcp_servers.herdr]`）に登録している
- 申告忘れの差し戻し: `~/.claude/hooks/herdr-status-guard.py`（claude と codex の両方の Stop hook から使う）。そのセッションに `herdr mcp` が付いていて、今のターンが始まった後に `set_status` が呼ばれていない（ペインの `phase_at` が古い）とき、または claude で waiting と申告したのに裏の処理も予約した確認も動いていないときだけ、1回だけ止めて申告し直させる。入れ子の `claude -p` と、MCP 登録前から動いているセッションは対象外
- codex の hook は信頼済みの印（`~/.codex/config.toml` の `trusted_hash`）が要る。`~/.codex/hooks.json` で hook の順番を変えると印が合わなくなり、codex が黙って hook を無視する
- fork 導入前のデータのバックアップ: `~/.config/herdr-backup-20261018`
