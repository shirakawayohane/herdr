//! fork: `herdr mcp`, a stdio MCP server that lets agents talk to Herdr.
//!
//! Agents started in a Herdr pane inherit `HERDR_PANE_ID`, and the MCP server
//! they launch inherits it in turn, so every tool acts on the agent's own pane.
//! Claude Code background sessions (`/fork`, `claude --bg`) are the exception:
//! they run inside Claude's daemon, which carries the pane id of whichever pane
//! started it, so their tools act on the pane running `claude attach <job>`
//! instead (`PaneSource`).
//! `set_status` reports how a turn ended, which drives the agent panel's
//! status sections (`crate::agent_group`). Reporting `waiting` requires
//! monitoring that is already running, and the `done` after a `waiting` is
//! kept as `question` so the user looks at the result. `update_board` keeps
//! what the user should see between turns on the pane's board
//! (`crate::board`), and every `set_status` reply points out board entries
//! that look stale, so agents keep it current without extra instructions.
//! Asks carry the detail the user needs to answer them; Herdr serves them as
//! the pane's question sheet (`crate::sheet`), and `update_board` returns its
//! URL and names the asks that still lack detail.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::agent_group::{
    PHASE_AT_TOKEN, PHASE_DONE, PHASE_NOTE_TOKEN, PHASE_QUESTION, PHASE_TOKEN, PHASE_WAITING,
};
use crate::api::schema::{
    Method, PaneBoardSetParams, PaneReportMetadataParams, PaneTarget, Request,
};
use crate::board::{AskKind, PaneBoard, TodoState};

const SOURCE: &str = "user:herdr-mcp";
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
pub(crate) const SET_STATUS_TOOL: &str = "set_status";
const UPDATE_BOARD_TOOL: &str = "update_board";

const INSTRUCTIONS: &str = "This session runs inside Herdr, a terminal workspace where the user \
watches many agent sessions at once. Before your final reply of every turn, call set_status \
once so the user can tell which sessions need them, which are waiting on something, and which \
can be closed. Keep the session's board current with update_board: what you need from the user \
(asks), the remaining steps (todos), and every URL the user may want to open again (links). \
Whenever you need an answer or an action from the user, put it on the board as an ask in the \
same turn. A question the user can answer at a glance from its one line just gets asked in your \
reply; one that needs background gets its detail on the ask, which Herdr shows on the session's \
question sheet, a page the user opens from the board to read the full background and write \
their answers, often much later and without rereading the conversation. Their answers arrive as a normal message that starts \
with \"質問シートへの回答\". The user reads the board and the sheet instead of scrolling back, so \
do not repeat them at the end of every reply.";

const UPDATE_BOARD_DESCRIPTION: &str = "Replace lists on this session's board, which Herdr shows \
beside the terminal until you change it. Each list you pass replaces that whole list; lists you \
leave out stay as they are; pass [] to clear one. The user reads the board, so write every entry \
for them: in their language, short enough to scan, about the work or the result rather than about \
you, and never naming the user in the third person (write \"Check the sidebar layout\", not \
\"Have the user check the layout\").\n\
- asks: everything you are waiting on the user for. kind decide = a decision or answer only they \
can give; kind do = something only they can do (sign in, review, run a command). Add them in the \
same turn your reply asks, numbered like your reply (\"Q1 ...\"), and remove each one once the \
user has answered or done it. text is the one-line question the board shows. Decide for each \
ask whether that line is enough. A simple question the user can answer at a glance, with \
nothing to look up or weigh (\"テスト通知は届いたか？\", \"このまま push してよいか？\" right \
after you showed the change), gets no detail (never a question about how something looks, \
which needs images in its detail): ask it in your reply and do not mention the \
sheet. Anything that needs background to answer well, such as a choice between options, a \
trade-off, or a question the user may read hours later without the conversation, gets detail: \
those asks are the session's question sheet, a web page where the user reads each one in full \
and writes the answer, and this call returns its URL. The sheet is where such questions get \
answered, not a copy of your reply: name each question in one line in your reply, leave its \
background to the sheet, and close with one sentence that says where to answer, with the URL \
(\"回答は質問シートからどうぞ: <URL>\"). Never print the URL on its own without saying what it \
is for, and leave it out when no ask has detail to read there, such as a reply that only \
reports results, a simple question, or a kind do the user just carries out. Answers written there come \
back to you as a message starting with \"質問シートへの回答\"; an answer may also ask you for \
more explanation, in which case improve the detail and keep the ask.\n\
- todos: the remaining steps of work with several steps, each named by what it achieves. Update \
states as you go and clear the list when the work is finished.\n\
- links: every URL the user may want to open again, so they never have to ask for it or search \
the conversation: a dev server you started, an artifact or document you published, a PR, a \
preview or deploy. Add one as soon as the URL exists, label it by what it shows, and remove it \
when it stops working, for example after stopping the server.";

const ASK_DETAIL_DESCRIPTION: &str = "Markdown shown under the question on the question sheet. \
Leave it out for a simple question the user can answer at a glance from the one line. Otherwise \
the user answers from the sheet alone, often hours later and between other sessions, so write it \
for someone who has forgotten this conversation; a question that leans on the conversation gets \
asked back instead of answered. Cover, in the user's language: what this is about and what you \
found, with the concrete facts (file names, numbers, error text, current behaviour); why it needs \
them and what it blocks; each option with what it leads to (cost, risk, what changes for them); \
which one you recommend and why; and what you will do if they have no preference. For kind do: \
the exact steps, commands or URLs, the thing itself when they are to check something (the full \
text of a draft, the lines that changed), and how they tell you it is done. Explain any name or label \
you made up during the work, and never write \"as discussed above\". Short paragraphs, lists, \
tables and code blocks render. Images render too, written as ![caption](path) with the path of \
a PNG, JPEG, GIF or WebP file on this machine (or an https URL). A question about how something \
looks (a layout, colours, spacing, wording on a screen, a before and after, which of several \
designs) is never a simple question and cannot be answered from words: show it. Take a \
screenshot of the real screen, or render a mockup when it does not exist yet, one image per \
option, each captioned with the name its option uses, plus the current state when the question \
is about a change. Keep the files until the ask is answered.";

const ASK_OPTIONS_DESCRIPTION: &str = "Answers the user can pick with one click. Each is a short \
phrase that is a complete answer on its own (\"はい、削除する\", not \"A\"). Put the one you \
recommend first and end it with \"(推奨)\". Leave options out when the answer is free text; the \
user can always add a note or write something else.";

const SET_STATUS_DESCRIPTION: &str = "Report how this turn ends. Call it exactly once, right \
before your final reply of every turn.\n\
- question: your reply asks the user something, or needs their decision or confirmation before \
you can continue.\n\
- waiting: nothing to ask now, but you are monitoring something whose result decides the next \
step, such as CI, a background task, or a scheduled check. Only use it after you have set up \
something that wakes you when the result is in (a background task, a monitor, or a scheduled \
check), and pass monitor_set=true and a note saying what you are waiting for.\n\
- done: the session can be closed now. Report it on your own only when the task was simple and \
is clearly finished, such as a one-off question answered or a small, verified change with \
nothing left to try. For anything bigger, or when the user may still want follow-up changes, \
end your reply by asking whether the task can be marked done and report question with a note \
such as \"完了確認\"; report done once the user agrees.\n\
After waiting, the next done is recorded as question so the user checks the result.";

const NOTE_DESCRIPTION: &str = "Short label shown next to the session, in the user's language, \
at most about 20 characters. Required for waiting (what you are waiting for, e.g. \"PR #12 \
のCI\"); optional for question (what you need from the user).";

const MONITOR_SET_DESCRIPTION: &str = "Required for waiting: true only if a background task, \
monitor, or scheduled check is already running that will wake you when the result is in.";

const MONITOR_REQUIRED: &str = "Not recorded. waiting needs monitoring that is already running: \
start a background task, a monitor, or a scheduled check that wakes you when the result is in, \
then call set_status again with monitor_set=true. If you cannot monitor it yourself, report \
question and tell the user what to check, or report done.";

const NO_PANE: &str = "not shown in any Herdr pane; nothing recorded";

const MAX_NOTE_CHARS: usize = 40;
const FINISHED_MONITORING_NOTE: &str = "監視終了・結果を確認";

pub(super) fn run_mcp_command(args: &[String]) -> std::io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        println!("usage: herdr mcp");
        println!();
        println!("Runs a stdio MCP server for agents inside Herdr panes.");
        println!("Register it with: claude mcp add -s user herdr -- herdr mcp");
        return Ok(0);
    }
    if !args.is_empty() {
        eprintln!("usage: herdr mcp");
        return Ok(2);
    }

    let mut source = PaneSource::from_env(
        std::env::var("HERDR_PANE_ID").ok(),
        std::env::var("CLAUDE_JOB_DIR").ok(),
    );
    let mut herdr = HerdrPanes;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => {
                // Only tool calls need the pane, and a background session's
                // pane can change between calls.
                let pane_id = (message.get("method").and_then(Value::as_str) == Some("tools/call"))
                    .then(|| source.pane_id())
                    .flatten();
                handle_message(&message, pane_id.as_deref(), &mut herdr)
            }
            Err(err) => Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {err}"),
            )),
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(0)
}

/// The pane state the tools read and write, behind a trait for tests.
trait PaneStatus {
    fn current_phase(&self, pane_id: &str) -> Option<String>;
    fn report(&mut self, pane_id: &str, phase: &str, note: Option<&str>) -> Result<(), String>;
    fn board(&self, pane_id: &str) -> Option<PaneBoard>;
    fn set_board(&mut self, params: PaneBoardSetParams) -> Result<(), String>;
}

/// Returns the JSON-RPC response for one message, or `None` for notifications.
fn handle_message(
    message: &Value,
    pane_id: Option<&str>,
    panes: &mut dyn PaneStatus,
) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "herdr", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [set_status_tool(), update_board_tool()] }),
        "tools/call" => call_tool(&params, pane_id, panes),
        _ => return Some(error_response(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn set_status_tool() -> Value {
    json!({
        "name": SET_STATUS_TOOL,
        "description": SET_STATUS_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": [PHASE_QUESTION, PHASE_WAITING, PHASE_DONE],
                },
                "note": { "type": "string", "description": NOTE_DESCRIPTION },
                "monitor_set": { "type": "boolean", "description": MONITOR_SET_DESCRIPTION },
            },
            "required": ["status"],
            "additionalProperties": false,
        },
    })
}

fn update_board_tool() -> Value {
    json!({
        "name": UPDATE_BOARD_TOOL,
        "description": UPDATE_BOARD_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "asks": {
                    "type": "array",
                    "maxItems": crate::board::MAX_ASKS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "enum": ["decide", "do"] },
                            "text": { "type": "string" },
                            "detail": { "type": "string", "description": ASK_DETAIL_DESCRIPTION },
                            "options": {
                                "type": "array",
                                "maxItems": crate::board::MAX_OPTIONS,
                                "items": { "type": "string" },
                                "description": ASK_OPTIONS_DESCRIPTION,
                            },
                        },
                        "required": ["kind", "text"],
                        "additionalProperties": false,
                    },
                },
                "todos": {
                    "type": "array",
                    "maxItems": crate::board::MAX_TODOS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": { "type": "string" },
                            "state": { "type": "string", "enum": ["pending", "in_progress", "done"] },
                        },
                        "required": ["text", "state"],
                        "additionalProperties": false,
                    },
                },
                "links": {
                    "type": "array",
                    "maxItems": crate::board::MAX_LINKS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string" },
                            "url": { "type": "string" },
                        },
                        "required": ["label", "url"],
                        "additionalProperties": false,
                    },
                },
            },
            "additionalProperties": false,
        },
    })
}

fn call_tool(params: &Value, pane_id: Option<&str>, panes: &mut dyn PaneStatus) -> Value {
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    match params.get("name").and_then(Value::as_str) {
        Some(SET_STATUS_TOOL) => set_status(&arguments, pane_id, panes),
        Some(UPDATE_BOARD_TOOL) => update_board(arguments, pane_id, panes),
        _ => tool_result("unknown tool", true),
    }
}

fn update_board(arguments: Value, pane_id: Option<&str>, panes: &mut dyn PaneStatus) -> Value {
    let Some(pane_id) = pane_id else {
        return tool_result(NO_PANE, false);
    };
    let mut arguments = match arguments {
        Value::Object(arguments) => arguments,
        Value::Null => serde_json::Map::new(),
        _ => return tool_result("arguments must be an object", true),
    };
    arguments.insert("pane_id".into(), Value::String(pane_id.to_string()));
    let mut params = match serde_json::from_value::<PaneBoardSetParams>(Value::Object(arguments)) {
        Ok(params) => params,
        Err(err) => return tool_result(&format!("invalid board: {err}"), true),
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    for ask in params.asks.iter_mut().flatten() {
        match with_local_images_resolved(&ask.detail, &cwd) {
            Ok(detail) => ask.detail = detail,
            Err(err) => return tool_result(&format!("Not recorded. {err}"), true),
        }
    }
    if params.asks.is_none() && params.todos.is_none() && params.links.is_none() {
        return tool_result("pass at least one of asks, todos, links", true);
    }
    if nested_agent() {
        return tool_result("nested agent; the pane's own agent keeps the board", false);
    }
    let asked = params.asks.as_ref().is_some_and(|asks| !asks.is_empty());
    let needs_answer = params.asks.as_ref().is_some_and(|asks| {
        asks.iter()
            .any(|ask| ask.kind != AskKind::Do && !ask.detail.trim().is_empty())
    });
    let bare = params.asks.as_deref().map(asks_without_detail);
    if let Err(err) = panes.set_board(params) {
        return tool_result(&format!("herdr did not accept the board: {err}"), true);
    }
    let mut lines = vec!["board updated".to_string()];
    if asked {
        match panes.board(pane_id).and_then(|board| board.sheet_url) {
            Some(url) if needs_answer => lines.push(format!(
                "Question sheet: {url}\nThe user reads the questions in full and answers them \
                 there. In your reply, name each question in one line without repeating its \
                 detail, and close with one sentence saying they can answer on the sheet, with \
                 this URL."
            )),
            // Nothing to read there before answering: the board's own link is enough.
            Some(url) => lines.push(format!(
                "Question sheet: {url}\nThe board links to it, so leave the URL out of your \
                 reply; none of these asks is a question with detail to read there."
            )),
            None => lines.push(
                "No question sheet is being served, so your reply has to carry the full detail."
                    .to_string(),
            ),
        }
    }
    lines.extend(bare.as_deref().and_then(missing_detail_hint));
    tool_result(&lines.join("\n"), false)
}

/// Rewrites the local images a detail embeds to absolute paths, which is what
/// the question sheet serves, and refuses ones it could not show.
fn with_local_images_resolved(detail: &str, cwd: &std::path::Path) -> Result<String, String> {
    let mut resolved = detail.to_string();
    for target in crate::board::image_targets(detail) {
        if target.starts_with("https://") || target.starts_with("data:") {
            continue;
        }
        if target.starts_with("http://") {
            return Err(format!(
                "The question sheet cannot show the image {target}: save it to a file and embed \
                 the file's path, or use an https URL."
            ));
        }
        let path = match target.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => cwd.join(target.strip_prefix("file://").unwrap_or(target)),
        };
        if crate::board::image_content_type(target).is_none() {
            return Err(format!(
                "The question sheet shows PNG, JPEG, GIF and WebP images; {target} is not one."
            ));
        }
        if !path.is_file() {
            return Err(format!(
                "The image {target} does not exist ({}). Create it first, and use a path \
                 without spaces.",
                path.display()
            ));
        }
        let absolute = path.display().to_string();
        if absolute.contains(char::is_whitespace) || absolute.contains(')') {
            return Err(format!(
                "The image path {absolute} contains a space or parenthesis; copy the file to a \
                 path without them."
            ));
        }
        resolved = resolved.replace(&format!("]({target})"), &format!("]({absolute})"));
    }
    Ok(resolved)
}

fn asks_without_detail(asks: &[crate::board::BoardAsk]) -> Vec<String> {
    asks.iter()
        .filter(|ask| ask.detail.trim().is_empty())
        .map(|ask| ask.text.trim().to_string())
        .collect()
}

fn missing_detail_hint(bare: &[String]) -> Option<String> {
    (!bare.is_empty()).then(|| {
        format!(
            "These asks have no detail: {}. That is right for a simple question the user can \
             answer at a glance. If one needs background to answer well, call update_board again \
             with detail the user can answer from without the conversation.",
            bare.join(" / ")
        )
    })
}

fn set_status(arguments: &Value, pane_id: Option<&str>, panes: &mut dyn PaneStatus) -> Value {
    let status = arguments
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !crate::agent_group::is_phase(status) {
        return tool_result("status must be one of: question, waiting, done", true);
    }
    let note = arguments
        .get("note")
        .and_then(Value::as_str)
        .map(|note| note.trim().chars().take(MAX_NOTE_CHARS).collect::<String>())
        .filter(|note| !note.is_empty());
    if status == PHASE_WAITING {
        if arguments.get("monitor_set").and_then(Value::as_bool) != Some(true) {
            return tool_result(MONITOR_REQUIRED, true);
        }
        if note.is_none() {
            return tool_result(
                "Not recorded. waiting needs a note saying what you are waiting for.",
                true,
            );
        }
    }
    let Some(pane_id) = pane_id else {
        return tool_result(NO_PANE, false);
    };
    if nested_agent() {
        // An agent started by another agent (for example `claude -p`) shares
        // the pane id but does not own the pane's status.
        return tool_result(
            "nested agent; the pane's own agent reports its status",
            false,
        );
    }
    // Finished monitoring is something the user should look at before the
    // session counts as closable.
    let (phase, note, message) =
        if status == PHASE_DONE && panes.current_phase(pane_id).as_deref() == Some(PHASE_WAITING) {
            (
            PHASE_QUESTION,
            note.or_else(|| Some(FINISHED_MONITORING_NOTE.to_string())),
            "recorded as question: monitoring has finished, so the user should check the result"
                .to_string(),
        )
        } else {
            (status, note, format!("reported {status}"))
        };
    match panes.report(pane_id, phase, note.as_deref()) {
        Ok(()) => {
            let hints = panes
                .board(pane_id)
                .map(|board| board_hints(phase, &board))
                .unwrap_or_default();
            tool_result(
                &[message]
                    .into_iter()
                    .chain(hints)
                    .collect::<Vec<_>>()
                    .join("\n"),
                false,
            )
        }
        Err(err) => tool_result(&format!("herdr did not accept the report: {err}"), true),
    }
}

/// Reminders about board entries that disagree with how the turn ended.
fn board_hints(phase: &str, board: &PaneBoard) -> Vec<String> {
    let mut hints = Vec::new();
    if board.asks.is_empty() {
        if phase == PHASE_QUESTION {
            hints.push(
                "Your reply waits on the user, but the board has no asks. Add what you need from \
                 them with update_board."
                    .to_string(),
            );
        }
    } else {
        let asks = board
            .asks
            .iter()
            .map(|ask| match ask.kind {
                AskKind::Do => format!("[do] {}", ask.text),
                _ => ask.text.clone(),
            })
            .collect::<Vec<_>>()
            .join(" / ");
        hints.push(format!(
            "Board asks still open: {asks}. Remove the ones the user has answered or done with \
             update_board."
        ));
    }
    let open_todos = board
        .todos
        .iter()
        .filter(|todo| todo.state != TodoState::Done)
        .count();
    if phase == PHASE_DONE && open_todos > 0 {
        hints.push(format!(
            "The work is done but {open_todos} board todos are not; mark them done or clear the \
             list with update_board."
        ));
    }
    hints
}

const AGENT_COMMANDS: &[&str] = &["claude", "codex"];

/// True when more than one agent process sits above this server.
#[cfg(unix)]
fn nested_agent() -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,comm="])
        .output()
    else {
        return false;
    };
    let table = String::from_utf8_lossy(&output.stdout);
    count_agent_ancestors(&table, std::process::id()) > 1
}

#[cfg(not(unix))]
fn nested_agent() -> bool {
    false
}

#[cfg_attr(not(unix), allow(dead_code))] // only the unix ps probe calls it
fn count_agent_ancestors(ps_table: &str, pid: u32) -> usize {
    let processes = ps_table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            Some((pid, (ppid, command)))
        })
        .collect::<HashMap<_, _>>();
    let mut count = 0;
    let mut current = pid;
    for _ in 0..64 {
        let Some((ppid, command)) = processes.get(&current) else {
            break;
        };
        let name = command.rsplit('/').next().unwrap_or(command);
        if AGENT_COMMANDS.contains(&name) {
            count += 1;
        }
        if *ppid <= 1 || *ppid == current {
            break;
        }
        current = *ppid;
    }
    count
}

/// Which pane this server's agent is shown in.
#[derive(Debug, PartialEq, Eq)]
enum PaneSource {
    /// The pane the agent was started in.
    Inherited(Option<String>),
    /// A Claude Code background session, shown wherever `claude attach <job>`
    /// runs, if anywhere. Never the inherited pane, which belongs to whatever
    /// started Claude's daemon. Remembers the last pane found.
    ClaudeJob { job: String, last: Option<String> },
}

impl PaneSource {
    fn from_env(pane_id: Option<String>, claude_job_dir: Option<String>) -> Self {
        let job = claude_job_dir.and_then(|dir| {
            std::path::Path::new(dir.trim_end_matches('/'))
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
                })
                .map(str::to_string)
        });
        match job {
            Some(job) => Self::ClaudeJob { job, last: None },
            None => Self::Inherited(pane_id.filter(|id| !id.is_empty())),
        }
    }

    fn pane_id(&mut self) -> Option<String> {
        match self {
            Self::Inherited(pane_id) => pane_id.clone(),
            Self::ClaudeJob { job, last } => {
                let still_attached = last
                    .as_deref()
                    .is_some_and(|pane_id| pane_attaches(pane_id, job));
                if !still_attached {
                    *last = attaching_pane(job);
                }
                last.clone()
            }
        }
    }
}

/// The pane running `claude attach <job>`. Only panes Herdr sees as Claude
/// are checked, one process lookup each.
fn attaching_pane(job: &str) -> Option<String> {
    let response = super::send_request_unchecked(&Request {
        id: "mcp:pane_list".into(),
        method: Method::PaneList(Default::default()),
    })
    .ok()?;
    response
        .pointer("/result/panes")?
        .as_array()?
        .iter()
        .filter(|pane| pane.get("agent").and_then(Value::as_str) == Some("claude"))
        .filter_map(|pane| pane.get("pane_id").and_then(Value::as_str))
        .find(|pane_id| pane_attaches(pane_id, job))
        .map(str::to_string)
}

fn pane_attaches(pane_id: &str, job: &str) -> bool {
    let Ok(response) = super::send_request_unchecked(&Request {
        id: "mcp:pane_process_info".into(),
        method: Method::PaneProcessInfo(crate::api::schema::PaneProcessInfoParams {
            pane_id: Some(pane_id.to_string()),
        }),
    }) else {
        return false;
    };
    response
        .pointer("/result/process_info/foreground_processes")
        .and_then(Value::as_array)
        .is_some_and(|processes| {
            processes.iter().any(|process| {
                process
                    .get("argv")
                    .and_then(Value::as_array)
                    .is_some_and(|argv| attaches_job(argv, job))
            })
        })
}

/// True for `claude attach <job>` (with any options before the job id).
fn attaches_job(argv: &[Value], job: &str) -> bool {
    let argv = argv.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    let name = argv
        .first()
        .map(|arg0| arg0.rsplit('/').next().unwrap_or(arg0));
    name == Some("claude")
        && argv
            .iter()
            .position(|arg| *arg == "attach")
            .is_some_and(|at| argv[at + 1..].contains(&job))
}

struct HerdrPanes;

impl PaneStatus for HerdrPanes {
    fn current_phase(&self, pane_id: &str) -> Option<String> {
        let response = super::send_request_unchecked(&Request {
            id: "mcp:pane_get".into(),
            method: Method::PaneGet(PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        })
        .ok()?;
        response
            .pointer(&format!("/result/pane/tokens/{PHASE_TOKEN}"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn board(&self, pane_id: &str) -> Option<PaneBoard> {
        let response = super::send_request_unchecked(&Request {
            id: "mcp:pane_get".into(),
            method: Method::PaneGet(PaneTarget {
                pane_id: pane_id.to_string(),
            }),
        })
        .ok()?;
        let board = response.pointer("/result/pane/board").cloned();
        // A pane without a board omits the field.
        board.map_or(Some(PaneBoard::default()), |board| {
            serde_json::from_value(board).ok()
        })
    }

    fn set_board(&mut self, params: PaneBoardSetParams) -> Result<(), String> {
        let response = super::send_request_unchecked(&Request {
            id: "mcp:update_board".into(),
            method: Method::PaneBoardSet(params),
        })
        .map_err(|err| err.to_string())?;
        match response.get("error") {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }

    fn report(&mut self, pane_id: &str, phase: &str, note: Option<&str>) -> Result<(), String> {
        let request = Request {
            id: "mcp:set_status".into(),
            method: Method::PaneReportMetadata(PaneReportMetadataParams {
                pane_id: pane_id.to_string(),
                source: SOURCE.into(),
                agent: None,
                applies_to_source: None,
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
                tokens: HashMap::from([
                    (PHASE_TOKEN.to_string(), Some(phase.to_string())),
                    (PHASE_AT_TOKEN.to_string(), Some(unix_millis().to_string())),
                    (PHASE_NOTE_TOKEN.to_string(), note.map(str::to_string)),
                ]),
                clear_title: false,
                clear_display_agent: false,
                clear_state_labels: false,
                seq: None,
                ttl_ms: None,
            }),
        };
        let response = super::send_request_unchecked(&request).map_err(|err| err.to_string())?;
        match response.get("error") {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakePanes {
        phase: Option<String>,
        reports: Vec<(String, String, Option<String>)>,
        board: PaneBoard,
        board_sets: Vec<PaneBoardSetParams>,
    }

    impl PaneStatus for FakePanes {
        fn current_phase(&self, _: &str) -> Option<String> {
            self.phase.clone()
        }

        fn report(&mut self, pane: &str, phase: &str, note: Option<&str>) -> Result<(), String> {
            self.reports
                .push((pane.into(), phase.into(), note.map(str::to_string)));
            Ok(())
        }

        fn board(&self, _: &str) -> Option<PaneBoard> {
            Some(self.board.clone())
        }

        fn set_board(&mut self, params: PaneBoardSetParams) -> Result<(), String> {
            self.board_sets.push(params);
            Ok(())
        }
    }

    fn call(panes: &mut FakePanes, pane: Option<&str>, name: &str, arguments: Value) -> Value {
        handle_message(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": {"name": name, "arguments": arguments}}),
            pane,
            panes,
        )
        .unwrap()["result"]
            .clone()
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn update_board_sends_only_the_lists_it_got_for_its_own_pane() {
        let mut panes = FakePanes::default();
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [{"kind": "decide", "text": "Q1 削除してよいか"}],
                   "links": []}),
        );
        assert_eq!(result["isError"], false);
        let [params] = panes.board_sets.as_slice() else {
            panic!("expected one board update");
        };
        assert_eq!(params.pane_id, "w1:p2");
        assert_eq!(params.asks.as_ref().map(Vec::len), Some(1));
        assert!(params.todos.is_none());
        assert_eq!(params.links.as_deref(), Some(&[][..]));
    }

    #[test]
    fn update_board_returns_the_sheet_and_names_asks_without_detail() {
        let mut panes = FakePanes::default();
        panes.board.sheet_url = Some("http://127.0.0.1:4100/s/w1:p2".into());
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [
                {"kind": "decide", "text": "Q1 削除してよいか",
                 "detail": "## 背景\n...", "options": ["はい (推奨)", "いいえ"]},
                {"kind": "do", "text": "Q2 stg に再ログイン", "detail": " "},
            ]}),
        );
        assert_eq!(result["isError"], false);
        let asks = panes.board_sets[0].asks.as_ref().unwrap();
        assert_eq!(asks[0].detail, "## 背景\n...");
        assert_eq!(asks[0].options.len(), 2);
        let reply = text(&result);
        assert!(
            reply.contains("Question sheet: http://127.0.0.1:4100/s/w1:p2"),
            "{reply}"
        );
        assert!(reply.contains("close with one sentence"), "{reply}");
        assert!(reply.contains("no detail"), "{reply}");
        assert!(reply.contains("Q2 stg に再ログイン") && !reply.contains("Q1 削除してよいか"));

        // Things to do are not questions, so the reply should not point at the sheet.
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [{"kind": "do", "text": "下書きを確認して送信", "detail": "全文"}]}),
        );
        assert!(
            text(&result).contains("leave the URL out"),
            "{}",
            text(&result)
        );

        // A simple question is asked in the reply; the sheet is not advertised for it.
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [{"kind": "decide", "text": "Q1 テスト通知は届いたか"}]}),
        );
        let reply = text(&result);
        assert!(reply.contains("leave the URL out"), "{reply}");
        assert!(reply.contains("right for a simple question"), "{reply}");

        // Clearing asks or touching other lists says nothing about the sheet.
        for arguments in [json!({"asks": []}), json!({"links": []})] {
            let result = call(&mut panes, Some("w1:p2"), UPDATE_BOARD_TOOL, arguments);
            assert_eq!(text(&result), "board updated");
        }

        panes.board.sheet_url = None;
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [{"kind": "decide", "text": "Q1", "detail": "d"}]}),
        );
        assert!(text(&result).contains("No question sheet"));
    }

    #[test]
    fn images_in_a_detail_become_absolute_paths_and_missing_ones_are_refused() {
        let dir = std::env::temp_dir().join(format!("herdr-mcp-img-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("shots")).unwrap();
        std::fs::write(dir.join("shots/a.png"), b"png").unwrap();
        let absolute = dir.join("shots/a.png").display().to_string();

        let detail = "![案A](shots/a.png)\n![外部](https://x.test/b.png)";
        assert_eq!(
            with_local_images_resolved(detail, &dir).unwrap(),
            format!("![案A]({absolute})\n![外部](https://x.test/b.png)")
        );
        let already = format!("![案A]({absolute})");
        assert_eq!(with_local_images_resolved(&already, &dir).unwrap(), already);
        assert_eq!(
            with_local_images_resolved("画像なし", &dir).unwrap(),
            "画像なし"
        );
        for bad in [
            "![x](shots/none.png)",
            "![x](shots/a.svg)",
            "![x](http://x.test/a.png)",
        ] {
            assert!(with_local_images_resolved(bad, &dir).is_err(), "{bad}");
        }

        let mut panes = FakePanes::default();
        let result = call(
            &mut panes,
            Some("w1:p2"),
            UPDATE_BOARD_TOOL,
            json!({"asks": [{"kind": "decide", "text": "Q1", "detail": "![x](/nonexistent/a.png)"}]}),
        );
        assert_eq!(result["isError"], true);
        assert!(panes.board_sets.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_board_rejects_empty_or_malformed_calls() {
        let mut panes = FakePanes::default();
        for arguments in [
            json!({}),
            json!({"todos": [{"state": "done"}]}),
            json!({"asks": "x"}),
        ] {
            let result = call(&mut panes, Some("w1:p2"), UPDATE_BOARD_TOOL, arguments);
            assert_eq!(result["isError"], true, "{result}");
        }
        assert!(panes.board_sets.is_empty());
    }

    #[test]
    fn set_status_points_out_board_entries_that_disagree_with_the_turn() {
        let mut panes = FakePanes::default();
        let result = set_status(&mut panes, Some("w1:p2"), json!({"status": "question"}));
        assert!(text(&result).contains("no asks"));

        panes.board = PaneBoard {
            asks: vec![crate::board::BoardAsk {
                kind: AskKind::Do,
                text: "stg に再ログイン".into(),
                ..Default::default()
            }],
            todos: vec![crate::board::BoardTodo {
                text: "PR 作成".into(),
                state: TodoState::Pending,
            }],
            ..PaneBoard::default()
        };
        let result = set_status(&mut panes, Some("w1:p2"), json!({"status": "done"}));
        assert!(text(&result).contains("[do] stg に再ログイン"));
        assert!(text(&result).contains("1 board todos"));

        panes.board = PaneBoard::default();
        let result = set_status(&mut panes, Some("w1:p2"), json!({"status": "done"}));
        assert_eq!(text(&result), "reported done");
    }

    fn set_status(panes: &mut FakePanes, pane: Option<&str>, arguments: Value) -> Value {
        handle_message(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "set_status", "arguments": arguments}}),
            pane,
            panes,
        )
        .unwrap()["result"]
            .clone()
    }

    #[test]
    fn initialize_echoes_protocol_version_and_lists_set_status() {
        let mut panes = FakePanes::default();
        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-03-26"}}),
            None,
            &mut panes,
        )
        .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");

        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            None,
            &mut panes,
        )
        .unwrap();
        assert_eq!(response["result"]["tools"][0]["name"], SET_STATUS_TOOL);
        assert_eq!(response["result"]["tools"][1]["name"], UPDATE_BOARD_TOOL);
    }

    #[test]
    fn notifications_get_no_response() {
        assert!(handle_message(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
            &mut FakePanes::default(),
        )
        .is_none());
    }

    #[test]
    fn waiting_requires_running_monitoring_and_a_note() {
        let mut panes = FakePanes::default();
        let result = set_status(
            &mut panes,
            Some("w1:p2"),
            json!({"status": "waiting", "note": "CI"}),
        );
        assert_eq!(result["isError"], true);
        let result = set_status(
            &mut panes,
            Some("w1:p2"),
            json!({"status": "waiting", "monitor_set": false, "note": "CI"}),
        );
        assert_eq!(result["isError"], true);
        let result = set_status(
            &mut panes,
            Some("w1:p2"),
            json!({"status": "waiting", "monitor_set": true}),
        );
        assert_eq!(result["isError"], true);
        assert!(panes.reports.is_empty());

        let result = set_status(
            &mut panes,
            Some("w1:p2"),
            json!({"status": "waiting", "monitor_set": true, "note": "PR #12のCI"}),
        );
        assert_eq!(result["isError"], false);
        assert_eq!(
            panes.reports,
            [("w1:p2".into(), "waiting".into(), Some("PR #12のCI".into()))]
        );
    }

    #[test]
    fn done_after_waiting_is_kept_as_question() {
        let mut panes = FakePanes {
            phase: Some("waiting".into()),
            ..FakePanes::default()
        };
        let result = set_status(&mut panes, Some("w1:p2"), json!({"status": "done"}));
        assert_eq!(result["isError"], false);
        assert_eq!(
            panes.reports,
            [(
                "w1:p2".into(),
                "question".into(),
                Some(FINISHED_MONITORING_NOTE.into())
            )]
        );

        let mut panes = FakePanes {
            phase: Some("question".into()),
            ..FakePanes::default()
        };
        set_status(
            &mut panes,
            Some("w1:p2"),
            json!({"status": "done", "note": " "}),
        );
        assert_eq!(panes.reports, [("w1:p2".into(), "done".into(), None)]);
    }

    #[test]
    fn set_status_rejects_unknown_status_and_is_harmless_outside_herdr() {
        let mut panes = FakePanes::default();
        let result = set_status(&mut panes, Some("w1:p2"), json!({"status": "maybe"}));
        assert_eq!(result["isError"], true);
        let result = set_status(&mut panes, None, json!({"status": "done"}));
        assert_eq!(result["isError"], false);
        assert!(panes.reports.is_empty());
    }

    #[test]
    fn agent_ancestors_are_counted_up_the_process_tree() {
        let table = "\
    1     0 /sbin/launchd
   10     1 /opt/homebrew/bin/fish
   20    10 /Users/me/.local/bin/claude
   30    20 /bin/zsh
   40    30 /Users/me/.local/bin/claude
   50    40 /Users/me/.local/bin/herdr
   60    20 /Users/me/.local/bin/herdr
";
        assert_eq!(count_agent_ancestors(table, 60), 1);
        assert_eq!(count_agent_ancestors(table, 50), 2);
        assert_eq!(count_agent_ancestors(table, 999), 0);
    }

    #[test]
    fn unknown_methods_get_method_not_found() {
        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 6, "method": "resources/list"}),
            None,
            &mut FakePanes::default(),
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn background_claude_sessions_ignore_the_inherited_pane() {
        assert_eq!(
            PaneSource::from_env(Some("w1:p2".into()), None),
            PaneSource::Inherited(Some("w1:p2".into()))
        );
        assert_eq!(
            PaneSource::from_env(Some(String::new()), None),
            PaneSource::Inherited(None)
        );
        assert_eq!(
            PaneSource::from_env(
                Some("w3:p1Q".into()),
                Some("/Users/me/.claude/jobs/eb9352dc/".into())
            ),
            PaneSource::ClaudeJob {
                job: "eb9352dc".into(),
                last: None
            }
        );
    }

    #[test]
    fn only_claude_attach_with_the_job_id_shows_the_job() {
        let argv = |args: &[&str]| args.iter().map(|arg| json!(arg)).collect::<Vec<_>>();
        assert!(attaches_job(
            &argv(&["claude", "attach", "eb9352dc"]),
            "eb9352dc"
        ));
        assert!(attaches_job(
            &argv(&["/usr/local/bin/claude", "attach", "--quiet", "eb9352dc"]),
            "eb9352dc"
        ));
        assert!(!attaches_job(
            &argv(&["claude", "attach", "fdc11042"]),
            "eb9352dc"
        ));
        assert!(!attaches_job(
            &argv(&["claude", "--resume", "eb9352dc"]),
            "eb9352dc"
        ));
        assert!(!attaches_job(
            &argv(&["herdr", "attach", "eb9352dc"]),
            "eb9352dc"
        ));
    }
}
