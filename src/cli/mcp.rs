//! fork: `herdr mcp`, a stdio MCP server that lets agents talk to Herdr.
//!
//! Agents started in a Herdr pane inherit `HERDR_PANE_ID`, and the MCP server
//! they launch inherits it in turn, so every tool acts on the agent's own pane.
//! The only tool today reports how a turn ended, which drives the agent
//! panel's status sections (`crate::agent_group`). Reporting `waiting`
//! requires monitoring that is already running, and the `done` after a
//! `waiting` is kept as `question` so the user looks at the result.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::agent_group::{
    PHASE_AT_TOKEN, PHASE_DONE, PHASE_NOTE_TOKEN, PHASE_QUESTION, PHASE_TOKEN, PHASE_WAITING,
};
use crate::api::schema::{Method, PaneReportMetadataParams, PaneTarget, Request};

const SOURCE: &str = "user:herdr-mcp";
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";
pub(crate) const SET_STATUS_TOOL: &str = "set_status";

const INSTRUCTIONS: &str = "This session runs inside Herdr, a terminal workspace where the user \
watches many agent sessions at once. Before your final reply of every turn, call set_status \
once so the user can tell which sessions need them, which are waiting on something, and which \
can be closed.";

const SET_STATUS_DESCRIPTION: &str = "Report how this turn ends. Call it exactly once, right \
before your final reply of every turn.\n\
- question: your reply asks the user something, or needs their decision or confirmation before \
you can continue.\n\
- waiting: nothing to ask now, but you are monitoring something whose result decides the next \
step, such as CI, a background task, or a scheduled check. Only use it after you have set up \
something that wakes you when the result is in (a background task, a monitor, or a scheduled \
check), and pass monitor_set=true and a note saying what you are waiting for.\n\
- done: the task is finished and nothing is pending, so the session can be closed.\n\
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

    let pane_id = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|id| !id.is_empty());
    let mut herdr = HerdrPanes;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(&message, pane_id.as_deref(), &mut herdr),
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
        "tools/list" => json!({ "tools": [set_status_tool()] }),
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

fn call_tool(params: &Value, pane_id: Option<&str>, panes: &mut dyn PaneStatus) -> Value {
    if params.get("name").and_then(Value::as_str) != Some(SET_STATUS_TOOL) {
        return tool_result("unknown tool", true);
    }
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
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
        return tool_result("not running inside a Herdr pane; nothing to report", false);
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
        Ok(()) => tool_result(&message, false),
        Err(err) => tool_result(&format!("herdr did not accept the report: {err}"), true),
    }
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
}
