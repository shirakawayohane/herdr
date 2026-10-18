//! fork: `herdr mcp`, a stdio MCP server that lets agents talk to Herdr.
//!
//! Agents started in a Herdr pane inherit `HERDR_PANE_ID`, and the MCP server
//! they launch inherits it in turn, so every tool acts on the agent's own pane.
//! The only tool today reports how a turn ended, which drives the agent
//! panel's status sections (`crate::agent_group`).

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::agent_group::{PHASE_DONE, PHASE_QUESTION, PHASE_TOKEN, PHASE_WAITING};
use crate::api::schema::{Method, PaneReportMetadataParams, Request};

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
- waiting: nothing to ask now, but you are waiting on something that decides the next step, \
such as CI, a background task, a scheduled check, or a review.\n\
- done: the task is finished and nothing is pending, so the session can be closed.";

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
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_message(&message, pane_id.as_deref(), report_phase),
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

/// Returns the JSON-RPC response for one message, or `None` for notifications.
fn handle_message(
    message: &Value,
    pane_id: Option<&str>,
    report: impl FnOnce(&str, &str) -> Result<(), String>,
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
        "tools/call" => call_tool(&params, pane_id, report),
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
                }
            },
            "required": ["status"],
            "additionalProperties": false,
        },
    })
}

fn call_tool(
    params: &Value,
    pane_id: Option<&str>,
    report: impl FnOnce(&str, &str) -> Result<(), String>,
) -> Value {
    if params.get("name").and_then(Value::as_str) != Some(SET_STATUS_TOOL) {
        return tool_result("unknown tool", true);
    }
    let status = params
        .get("arguments")
        .and_then(|arguments| arguments.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !crate::agent_group::is_phase(status) {
        return tool_result("status must be one of: question, waiting, done", true);
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
    match report(pane_id, status) {
        Ok(()) => tool_result(&format!("reported {status}"), false),
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

fn report_phase(pane_id: &str, status: &str) -> Result<(), String> {
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
            tokens: HashMap::from([(PHASE_TOKEN.to_string(), Some(status.to_string()))]),
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

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_report(_: &str, _: &str) -> Result<(), String> {
        panic!("must not report")
    }

    #[test]
    fn initialize_echoes_protocol_version_and_lists_set_status() {
        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-03-26"}}),
            None,
            no_report,
        )
        .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");

        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            None,
            no_report,
        )
        .unwrap();
        assert_eq!(response["result"]["tools"][0]["name"], SET_STATUS_TOOL);
    }

    #[test]
    fn notifications_get_no_response() {
        assert!(handle_message(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
            no_report,
        )
        .is_none());
    }

    #[test]
    fn set_status_reports_the_phase_for_the_own_pane() {
        let mut reported = None;
        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "set_status", "arguments": {"status": "waiting"}}}),
            Some("w1:p2"),
            |pane, status| {
                reported = Some((pane.to_string(), status.to_string()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(reported, Some(("w1:p2".into(), "waiting".into())));
    }

    #[test]
    fn set_status_rejects_unknown_status_and_is_harmless_outside_herdr() {
        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": {"name": "set_status", "arguments": {"status": "maybe"}}}),
            Some("w1:p2"),
            no_report,
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], true);

        let response = handle_message(
            &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                    "params": {"name": "set_status", "arguments": {"status": "done"}}}),
            None,
            no_report,
        )
        .unwrap();
        assert_eq!(response["result"]["isError"], false);
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
            no_report,
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }
}
