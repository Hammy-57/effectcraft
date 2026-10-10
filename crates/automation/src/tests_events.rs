//! MCP change notifications (`resources/subscribe`) and idempotent tool calls (`requestId`).

use serde_json::{Value, json};

use crate::{Backend, McpServer, Session};

fn server() -> McpServer {
    McpServer::new(Backend::headless(Session::default()))
}

fn rpc(s: &mut McpServer, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    serde_json::from_str(&s.handle_line(&line).unwrap()).unwrap()
}

fn revision(s: &mut McpServer) -> u64 {
    s.backend().session().unwrap().revision
}

#[test]
fn subscribed_document_reports_changes() {
    let mut s = server();
    let init = rpc(&mut s, "initialize", json!({}));
    assert_eq!(init["result"]["capabilities"]["resources"]["subscribe"], true);
    // Nothing is sent before a subscription.
    rpc(&mut s, "tools/call", json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {"name": "A"}}}));
    assert!(s.take_notifications().is_empty());

    let r = rpc(&mut s, "resources/subscribe", json!({"uri": "effectcraft://document"}));
    assert_eq!(r["result"], json!({}), "{r}");
    rpc(&mut s, "tools/call", json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {"name": "B"}}}));
    let notes = s.take_notifications();
    assert_eq!(notes, [json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "effectcraft://document"}})]);
    // A read changes nothing.
    rpc(&mut s, "tools/call", json!({"name": "doc_inspect", "arguments": {}}));
    assert!(s.take_notifications().is_empty());

    rpc(&mut s, "resources/unsubscribe", json!({"uri": "effectcraft://document"}));
    rpc(&mut s, "tools/call", json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {"name": "C"}}}));
    assert!(s.take_notifications().is_empty());

    let r = rpc(&mut s, "resources/subscribe", json!({"uri": "effectcraft://nope"}));
    assert_eq!(r["error"]["code"], -32002, "{r}");
}

#[test]
fn notifications_follow_the_reply_on_stdio() {
    let mut s = server();
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "resources/subscribe", "params": {"uri": "effectcraft://document"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "command_run", "arguments": {"id": "comp.new", "params": {}}}}),
    ]
    .iter()
    .map(|m| format!("{m}\n"))
    .collect::<String>();
    let mut out = Vec::new();
    s.serve(std::io::Cursor::new(input), &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[1]["id"], 2);
    assert_eq!(lines[2]["method"], "notifications/resources/updated");
}

/// A retried tool call with the same request id returns the first result and doesn't run again
/// (no duplicate "Comp 2"); `request_id` is not an unknown argument.
#[test]
fn a_repeated_request_id_runs_once() {
    let mut s = server();
    let call = json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {"name": "Dup"}}, "_meta": {"requestId": "agent-1"}});
    let first = rpc(&mut s, "tools/call", call.clone());
    assert_eq!(first["result"]["isError"], false, "{first}");
    let rev = revision(&mut s);
    let again = rpc(&mut s, "tools/call", call);
    assert_eq!(again["result"], first["result"]);
    assert_eq!(revision(&mut s), rev, "the retry must not run");

    let call = json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {}, "request_id": "agent-2"}});
    let first = rpc(&mut s, "tools/call", call.clone());
    assert_eq!(first["result"]["isError"], false, "{first}");
    let rev = revision(&mut s);
    assert_eq!(rpc(&mut s, "tools/call", call)["result"], first["result"]);
    assert_eq!(revision(&mut s), rev);
    // Another id runs.
    rpc(&mut s, "tools/call", json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {}, "request_id": "agent-3"}}));
    assert!(revision(&mut s) > rev);
}
