//! The server through its message loop, as an editor drives it: an in-memory connection, a
//! small project on disk, and the notifications and replies in the order they arrive.

use super::{run_with, Hooks};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{PublishDiagnosticsParams, Url};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

const PKG: &str = "package p;
  typedef enum { IDLE, BUSY } state_e;
  function automatic int twice(int x);
    return 2 * x;
  endfunction
endpackage
";

const TOP: &str = "module top;
  import p::*;
  state_e s;
  int n = twice(3);
  initial s = IDLE;
endmodule
";

/// A fresh project directory with `files` and a build file listing the `.sv` ones.
fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dvicode-lsp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".dvicode")).unwrap();
    let mut build = String::new();
    for (f, text) in files {
        std::fs::write(root.join(f), text).unwrap();
        if f.ends_with(".sv") {
            build.push_str(f);
            build.push('\n');
        }
    }
    std::fs::write(root.join(".dvicode/default.build"), build).unwrap();
    root
}

fn uri(root: &Path, f: &str) -> Url {
    Url::from_file_path(root.join(f)).unwrap()
}

/// LSP position of the first `needle` in `text` (ASCII text: columns are UTF-16 units too).
fn at(text: &str, needle: &str) -> Value {
    let off = text.find(needle).unwrap_or_else(|| panic!("no '{needle}'"));
    let line = text[..off].matches('\n').count();
    let col = off - text[..off].rfind('\n').map_or(0, |i| i + 1);
    json!({ "line": line, "character": col })
}

/// The editor side of the connection.
struct Client {
    conn: Connection,
    server: Option<JoinHandle<()>>,
    next_id: i32,
    /// Notifications not consumed yet, in arrival order.
    notes: Vec<Notification>,
    /// Every problem publication, in arrival order.
    published: Vec<PublishDiagnosticsParams>,
    /// Replies not consumed yet.
    replies: Vec<Response>,
    /// Requests of the server, in arrival order (each is acknowledged).
    requests: Vec<Request>,
}

impl Client {
    /// Starts a server on `root` and waits for its first build.
    fn start(root: &Path, options: Value) -> (Client, Value) {
        Client::start_with(root, options, Hooks::default())
    }

    fn start_with(root: &Path, options: Value, hooks: Hooks) -> (Client, Value) {
        // As VS Code: versioned document edits.
        Client::start_full(root, options, hooks, json!({ "workspace": { "workspaceEdit": { "documentChanges": true } } }))
    }

    /// [`Client::start_with`] for a client with `capabilities`.
    fn start_full(root: &Path, options: Value, hooks: Hooks, capabilities: Value) -> (Client, Value) {
        // The server's panic hook (internal error locations), and the test harness's after it.
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            let harness = std::panic::take_hook();
            crate::crash::install_hook();
            let server = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                server(info);
                harness(info);
            }));
        });
        let (server, conn) = Connection::memory();
        let handle = std::thread::spawn(move || run_with(server, hooks).expect("server loop"));
        let mut c = Client { conn, server: Some(handle), next_id: 0, notes: Vec::new(), published: Vec::new(), replies: Vec::new(), requests: Vec::new() };
        let root_uri = Url::from_file_path(root).unwrap();
        let init = c.request_ok(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": "test" }],
                "capabilities": capabilities,
                "initializationOptions": options,
            }),
        );
        c.notify("initialized", json!({}));
        c.wait_note("dvicode/status", |_| true);
        (c, init)
    }

    /// A round trip through the server's message loop, answered there (not on a worker
    /// thread, which a test may hold): the server has read everything sent before.
    fn sync(&mut self) {
        self.request_ok("textDocument/documentLink", json!({ "textDocument": { "uri": "file:///nowhere/x.sv" } }));
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn.sender.send(Message::Notification(Notification::new(method.into(), params))).unwrap();
    }

    fn recv(&mut self, deadline: Instant) -> Option<Message> {
        let left = deadline.saturating_duration_since(Instant::now());
        let m = self.conn.receiver.recv_timeout(left).ok()?;
        match &m {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                self.published.push(serde_json::from_value(n.params.clone()).unwrap());
            }
            // Requests of the server (none expected without client capabilities) are acknowledged
            // and kept.
            Message::Request(r) => {
                self.conn.sender.send(Message::Response(Response::new_ok(r.id.clone(), Value::Null))).unwrap();
                self.requests.push(r.clone());
            }
            _ => {}
        }
        Some(m)
    }

    fn request(&mut self, method: &str, params: Value) -> Response {
        let id = self.send_request(method, params);
        self.reply(&id, WAIT).unwrap_or_else(|| panic!("no reply to {method}"))
    }

    /// Sends a request without waiting for its reply.
    fn send_request(&mut self, method: &str, params: Value) -> RequestId {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.conn.sender.send(Message::Request(Request::new(id.clone(), method.into(), params))).unwrap();
        id
    }

    /// The reply to request `id`, if it comes within `wait`.
    fn reply(&mut self, id: &RequestId, wait: Duration) -> Option<Response> {
        if let Some(i) = self.replies.iter().position(|r| &r.id == id) {
            return Some(self.replies.remove(i));
        }
        let deadline = Instant::now() + wait;
        loop {
            match self.recv(deadline) {
                Some(Message::Response(r)) if &r.id == id => return Some(r),
                Some(Message::Response(r)) => self.replies.push(r),
                Some(Message::Notification(n)) => self.notes.push(n),
                Some(_) => {}
                None => return None,
            }
        }
    }

    fn request_ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.request(method, params);
        assert!(r.error.is_none(), "{method}: {:?}", r.error);
        r.result.unwrap_or(Value::Null)
    }

    /// The first notification `method` (already received or to come) whose params satisfy `pred`.
    fn wait_note(&mut self, method: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.notes.iter().position(|n| n.method == method && pred(&n.params)) {
            return self.notes.remove(i).params;
        }
        let deadline = Instant::now() + WAIT;
        loop {
            match self.recv(deadline) {
                Some(Message::Notification(n)) if n.method == method && pred(&n.params) => return n.params,
                Some(Message::Notification(n)) => self.notes.push(n),
                Some(_) => {}
                None => panic!("no {method} notification"),
            }
        }
    }

    /// Waits for problems of `uri` that satisfy `pred`.
    fn wait_problems(&mut self, uri: &Url, pred: impl Fn(&PublishDiagnosticsParams) -> bool) -> PublishDiagnosticsParams {
        let uri = uri.clone();
        let p = self.wait_note("textDocument/publishDiagnostics", |v| {
            let p: PublishDiagnosticsParams = serde_json::from_value(v.clone()).unwrap();
            p.uri == uri && pred(&p)
        });
        serde_json::from_value(p).unwrap()
    }

    /// The versions of the problem publications of `uri` so far.
    fn versions(&self, uri: &Url) -> Vec<Option<i32>> {
        self.published.iter().filter(|p| &p.uri == uri).map(|p| p.version).collect()
    }

    fn open(&self, uri: &Url, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "systemverilog", "version": version, "text": text } }),
        );
    }

    fn change(&self, uri: &Url, version: i32, text: &str) {
        self.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }));
    }

    fn shutdown(mut self) {
        self.request_ok("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        self.server.take().unwrap().join().expect("server thread");
    }
}

fn codes(p: &PublishDiagnosticsParams) -> Vec<String> {
    p.diagnostics
        .iter()
        .map(|d| match &d.code {
            Some(lsp_types::NumberOrString::String(s)) => s.clone(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn a_change_of_the_build_file_in_use_is_announced() {
    let root = project("bfswitch", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let message = |v: &Value| v["message"].as_str().unwrap_or("").to_string();
    // Removed: the next build configures the project automatically.
    let bf = root.join(".dvicode/default.build");
    let saved = std::fs::read_to_string(&bf).unwrap();
    std::fs::remove_file(&bf).unwrap();
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": uri(&root, ".dvicode/default.build"), "type": 3 }] }));
    let m = c.wait_note("window/showMessage", |_| true);
    assert!(message(&m).contains("configures the project automatically"), "{m}");
    // Created again: it is used, and the user is told so.
    std::fs::write(&bf, saved).unwrap();
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": uri(&root, ".dvicode/default.build"), "type": 1 }] }));
    let m = c.wait_note("window/showMessage", |_| true);
    assert!(message(&m).contains("now uses the build file") && message(&m).contains("default.build"), "{m}");
    c.shutdown();
}

#[test]
fn an_open_build_file_is_checked_as_it_is_typed() {
    let root = project("bfcheck", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let bf = uri(&root, ".dvicode/default.build");
    let text = "pkg.sv\ntop.sv\n-incdr inc\nmisng.sv\n";
    c.notify("textDocument/didOpen", json!({ "textDocument": { "uri": bf, "languageId": "dvicode-build", "version": 1, "text": text } }));
    let p = c.wait_problems(&bf, |p| p.version == Some(1));
    let found: Vec<(String, Value)> = p.diagnostics.iter().map(|d| (d.message.clone(), serde_json::to_value(d.range.start).unwrap())).collect();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].0.contains("did you mean -incdir?") && found[0].1 == at(text, "-incdr"), "{found:?}");
    assert!(found[2].0.starts_with("File not found") && found[2].1 == at(text, "misng"), "{found:?}");
    // What a path resolves to, and a link to it.
    let h = c.request_ok("textDocument/hover", json!({ "textDocument": { "uri": bf }, "position": at(text, "kg.sv") }));
    let md = h["contents"]["value"].as_str().unwrap_or("");
    assert!(md.contains("source file") && md.contains("pkg.sv") && md.contains("found"), "{h}");
    let links = c.request_ok("textDocument/documentLink", json!({ "textDocument": { "uri": bf } }));
    assert_eq!(links.as_array().map(|l| l.len()), Some(2), "{links}");
    // Links asked for right after an edit are those of the new text.
    c.change(&bf, 2, "top.sv\n");
    let links = c.request_ok("textDocument/documentLink", json!({ "textDocument": { "uri": bf } }));
    let targets: Vec<&str> = links.as_array().into_iter().flatten().filter_map(|l| l["target"].as_str()).collect();
    assert!(targets.len() == 1 && targets[0].ends_with("top.sv"), "{links}");
    // Fixed as typed: the problems go, before the file is saved.
    c.change(&bf, 3, "pkg.sv\ntop.sv\n");
    c.wait_problems(&bf, |p| p.version == Some(3) && p.diagnostics.is_empty());
    // Nothing else applies to it.
    assert_eq!(c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": bf } })), Value::Null);
    c.shutdown();
}

#[test]
fn an_open_argument_file_is_checked_the_way_the_build_reads_it() {
    let root = project("argcheck", &[("pkg.sv", PKG)]);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/top.sv"), TOP).unwrap();
    std::fs::write(root.join("sub/list.f"), "top.sv\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "+dvicode_setenv+SUB=$DVICODE_PROJECT_LOC/sub\npkg.sv\n-f sub/list.f\n").unwrap();
    let (mut c, _) = Client::start(&root, Value::Null);
    let f = uri(&root, "sub/list.f");
    // Relative to its own folder, with the build file's variables.
    let text = "top.sv\n$SUB/top.sv\ngone.sv\n";
    c.notify("textDocument/didOpen", json!({ "textDocument": { "uri": f, "languageId": "dvicode-build", "version": 1, "text": text } }));
    let p = c.wait_problems(&f, |p| p.version == Some(1));
    let messages: Vec<&str> = p.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("File not found") && messages[0].contains("gone.sv"), "{messages:?}");
    c.shutdown();
}

#[test]
fn initialize_announces_capabilities_and_builds() {
    let root = project("init", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (c, init) = Client::start(&root, Value::Null);
    let caps = &init["capabilities"];
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["textDocumentSync"]["change"], 2, "incremental sync");
    assert_eq!(caps["renameProvider"]["prepareProvider"], true);
    assert_eq!(caps["typeHierarchyProvider"], true);
    assert_eq!(init["serverInfo"]["name"], "dvicode-server");
    let mut c = c;
    let st = c.request_ok("dvicode/status", Value::Null);
    assert_eq!(st["top_files"], 2, "{st}");
    assert_eq!(st["modules"], 1, "{st}");
    // A clean project publishes nothing.
    assert!(c.published.is_empty(), "{:?}", c.published);
    c.shutdown();
}

#[test]
fn problems_follow_edits_and_never_go_back_to_an_older_version() {
    let root = project("edits", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let checks = Arc::new(Checks::default());
    let (mut c, _) = Client::start_with(&root, Value::Null, checks.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    // Three quick edits, within the debounce: an error appears, changes, and is fixed later.
    let broken = TOP.replace("initial s = IDLE;", "initial s = IDLE;\n  initial undeclared_a = 1;");
    c.change(&top, 2, &broken);
    let broken2 = broken.replace("undeclared_a", "undeclared_b");
    c.change(&top, 3, &broken2);
    let p = c.wait_problems(&top, |p| p.version == Some(3));
    assert_eq!(codes(&p), vec!["UNDECLARED_IDENTIFIER"]);
    assert!(p.diagnostics[0].message.contains("undeclared_b"), "{:?}", p.diagnostics);
    assert_eq!(p.diagnostics[0].range.start.line, 5);
    c.change(&top, 4, TOP);
    let p = c.wait_problems(&top, |p| p.version == Some(4));
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    // Every check done, everything they published is in.
    checks.wait_done(&mut c);
    let v = c.versions(&top);
    assert!(v.windows(2).all(|w| w[0] <= w[1]), "publication versions {v:?}");
    c.shutdown();
}

#[test]
fn requests_see_edits_made_just_before() {
    let root = project("flush", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    // No wait for the debounce: the request must see the new declaration.
    let edited = TOP.replace("  state_e s;", "  int fresh;\n  state_e s;\n  initial fresh = 2;");
    c.change(&top, 2, &edited);
    let r = c.request_ok("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(&edited, "fresh = 2") }));
    let locs = r.as_array().unwrap();
    assert_eq!(locs.len(), 1, "{r}");
    assert_eq!(locs[0]["uri"], top.as_str());
    assert_eq!(locs[0]["range"]["start"]["line"], 2);
    c.shutdown();
}

#[test]
fn semantic_token_changes_rebuild_the_full_answer() {
    let root = project("tokdelta", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, init) = Client::start(&root, Value::Null);
    assert_eq!(init["capabilities"]["semanticTokensProvider"]["full"]["delta"], true, "{init}");
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    let doc = json!({ "textDocument": { "uri": top } });
    let first = c.request_ok("textDocument/semanticTokens/full", doc.clone());
    let again = c.request_ok("textDocument/semanticTokens/full", doc.clone());
    assert_eq!(first["resultId"], again["resultId"], "one snapshot, one answer");
    let same = c.request_ok("textDocument/semanticTokens/full/delta", json!({ "textDocument": { "uri": top }, "previousResultId": first["resultId"] }));
    assert_eq!(same["edits"], json!([]), "{same}");
    let edited = TOP.replace("  state_e s;", "  int fresh;
  state_e s;
  initial fresh = 2;");
    c.change(&top, 2, &edited);
    let delta = c.request_ok("textDocument/semanticTokens/full/delta", json!({ "textDocument": { "uri": top }, "previousResultId": first["resultId"] }));
    let full = c.request_ok("textDocument/semanticTokens/full", doc.clone());
    assert_eq!(delta["resultId"], full["resultId"], "{delta} {full}");
    let mut data: Vec<Value> = first["data"].as_array().unwrap().clone();
    let edits = delta["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1, "{delta}");
    let (start, del) = (edits[0]["start"].as_u64().unwrap() as usize, edits[0]["deleteCount"].as_u64().unwrap() as usize);
    let ins = edits[0]["data"].as_array().cloned().unwrap_or_default();
    assert!(ins.len() < full["data"].as_array().unwrap().len(), "only the changed part is sent");
    data.splice(start..start + del, ins);
    assert_eq!(Value::Array(data), full["data"]);
    // A result the server does not know: the whole answer.
    let unknown = c.request_ok("textDocument/semanticTokens/full/delta", json!({ "textDocument": { "uri": top }, "previousResultId": "nope" }));
    assert_eq!(unknown["data"], full["data"]);
    c.shutdown();
}

#[test]
fn inlay_hints_follow_their_settings_without_a_rebuild() {
    let m = "module c(input logic clk, input logic d);\nendmodule\nmodule m;\n  logic k, v;\n  c u(k, v);\nendmodule\n";
    let root = project("hints", &[("m.sv", m)]);
    let (mut c, init) = Client::start(&root, Value::Null);
    assert_eq!(init["capabilities"]["inlayHintProvider"], true, "{init}");
    let uri = uri(&root, "m.sv");
    c.open(&uri, 1, m);
    let range = json!({ "start": { "line": 0, "character": 0 }, "end": { "line": 6, "character": 0 } });
    let labels = |c: &mut Client| -> Vec<String> {
        let r = c.request_ok("textDocument/inlayHint", json!({ "textDocument": { "uri": uri }, "range": range }));
        r.as_array().unwrap().iter().map(|h| h["label"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(labels(&mut c), ["clk:", "d:"]);
    c.notify("workspace/didChangeConfiguration", json!({ "settings": { "dvicode": { "inlayHints": { "ports": false } } } }));
    assert!(labels(&mut c).is_empty());
    c.shutdown();
}

#[test]
fn the_register_model_view_finds_registers_and_their_fields() {
    let ral = "virtual class uvm_reg_block; endclass
virtual class uvm_reg; endclass
class uvm_reg_field; endclass
class uvm_reg_map; endclass
`define CTRL_ADDR 'h10
class ctrl_reg extends uvm_reg;
  rand uvm_reg_field en;
  function new(string name = \"ctrl_reg\"); super.new(name, 32, 0); endfunction
  virtual function void build();
    en = uvm_reg_field::type_id::create(\"en\");
    en.configure(this, 2, 4, \"RW\", 0, 2'h1, 1, 1, 0);
  endfunction
endclass
class blk extends uvm_reg_block;
  rand ctrl_reg ctrl;
  function new(string name = \"blk\"); super.new(name); endfunction
  virtual function void build();
    default_map = create_map(\"bus\", 'hd000, 4, UVM_LITTLE_ENDIAN);
    ctrl = ctrl_reg::type_id::create(\"ctrl\");
    ctrl.configure(this);
    ctrl.build();
    default_map.add_reg(ctrl, `CTRL_ADDR);
  endfunction
endclass
";
    let root = project("registers", &[("ral.sv", ral)]);
    let (mut c, init) = Client::start(&root, Value::Null);
    assert_eq!(init["capabilities"]["experimental"]["dvicode"]["registers"], 1, "{init}");
    let t = c.request_ok("dvicode/tree", json!({ "view": "registers", "top": null, "filter": null, "reveal": null }));
    assert_eq!(t["top"], "blk", "{t}");
    assert_eq!(t["tops"], json!(["blk"]));
    let bus = &t["roots"][0]["children"][0];
    let ctrl = &bus["children"][0];
    assert_eq!((bus["name"].as_str(), ctrl["name"].as_str(), ctrl["address"].as_str()), (Some("bus"), Some("ctrl"), Some("0xd010")), "{t}");
    let f = c.request_ok("dvicode/registerFields", json!({ "top": "blk", "register": ctrl["register"], "revision": t["revision"] }));
    let en = &f["fields"][0];
    assert_eq!((en["name"].as_str(), en["range"].as_str(), en["access"].as_str(), en["reset"].as_str()), (Some("en"), Some("[5:4]"), Some("RW"), Some("0x1")), "{f}");
    let stale = c.request_ok("dvicode/registerFields", json!({ "top": "blk", "register": 0, "revision": -1 }));
    assert_eq!(stale["stale"], true);
    // A hexadecimal address may start with `d`; decimal ones are written `'d`.
    for a in ["d010", "0xD010", "32'hd010", "'d53264"] {
        let r = c.request_ok("dvicode/registerAt", json!({ "top": null, "address": a }));
        assert_eq!(r["hits"][0]["path"], json!(["blk", "bus", "ctrl"]), "{a}: {r}");
    }
    let r = c.request("dvicode/registerAt", json!({ "top": null, "address": "8'q1" }));
    assert!(r.error.is_some());
    c.shutdown();
}

#[test]
fn assertions_and_coverage_of_a_signal_are_asked_by_position() {
    let m = "module m(input logic clk, input logic req);
  a_req: assert property (@(posedge clk) req);
  logic unused;
endmodule
";
    let root = project("checksof", &[("m.sv", m)]);
    let (mut c, init) = Client::start(&root, Value::Null);
    assert_eq!(init["capabilities"]["experimental"]["dvicode"]["checks"], 1, "{init}");
    let uri = uri(&root, "m.sv");
    let r = c.request_ok("dvicode/checksOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "req)") }));
    assert_eq!(r["target"]["name"], "req", "{r}");
    assert_eq!((r["hits"][0]["label"].as_str(), r["hits"][0]["roles"][0].as_str(), r["hits"][0]["group"].as_str()), (Some("a_req"), Some("checked"), Some("checks")), "{r}");
    assert_eq!(r["limits"], json!([]));
    let none = c.request_ok("dvicode/checksOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "unused") }));
    assert_eq!(none["hits"], json!([]), "a declaration with no use: an empty result");
    let nothing = c.request_ok("dvicode/checksOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "endmodule") }));
    assert!(nothing.is_null(), "no declaration there");
    c.shutdown();
}

#[test]
fn writes_and_reads_of_a_signal_are_asked_by_position() {
    let m = "module m(input logic clk, output logic q);
  logic d;
  assign d = ~q;
  always_ff @(posedge clk) q <= d;
endmodule
";
    let root = project("accessesof", &[("m.sv", m)]);
    let (mut c, init) = Client::start(&root, Value::Null);
    assert_eq!(init["capabilities"]["experimental"]["dvicode"]["accesses"], 1, "{init}");
    let uri = uri(&root, "m.sv");
    let w = c.request_ok("dvicode/accessesOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "d;"), "mode": "writes" }));
    assert_eq!((w["target"]["name"].as_str(), w["mode"].as_str()), (Some("d"), Some("writes")), "{w}");
    assert_eq!((w["sites"][0]["kind"].as_str(), w["sites"][0]["group"].as_str(), w["sites"][0]["line"].as_u64()), (Some("continuous"), Some("writes"), Some(2)), "{w}");
    let r = c.request_ok("dvicode/accessesOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "d;"), "mode": "reads" }));
    assert_eq!((r["sites"][0]["kind"].as_str(), r["sites"][0]["process"].as_str(), r["sites"][0]["detail"].as_str()), (Some("read"), Some("always_ff"), Some("always_ff @(posedge clk) q <= d;")), "{r}");
    let nothing = c.request_ok("dvicode/accessesOf", json!({ "textDocument": { "uri": uri }, "position": at(m, "endmodule"), "mode": "reads" }));
    assert!(nothing.is_null(), "no signal there");
    c.shutdown();
}

#[test]
fn formatting_gives_whitespace_edits_by_the_settings_and_the_project_file() {
    let m = "module m;\nlogic a;   \n  logic b;\nendmodule\n";
    let root = project("formatting", &[("m.sv", m)]);
    let (mut c, init) = Client::start(&root, json!({ "format": { "indentSize": 3 } }));
    assert_eq!(init["capabilities"]["documentFormattingProvider"], true, "{init}");
    assert_eq!(init["capabilities"]["documentRangeFormattingProvider"], true, "{init}");
    let uri = uri(&root, "m.sv");
    let options = json!({ "tabSize": 8, "insertSpaces": true });
    let edits = c.request_ok("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": options }));
    assert_eq!(
        edits,
        json!([
            { "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 0 } }, "newText": "   " },
            { "range": { "start": { "line": 1, "character": 8 }, "end": { "line": 1, "character": 11 } }, "newText": "" },
            { "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 2 } }, "newText": "   " }
        ]),
        "the setting's 3 spaces over the editor's tab size"
    );
    // A range: the lines it touches only.
    let edits = c.request_ok("textDocument/rangeFormatting", json!({ "textDocument": { "uri": uri }, "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 3, "character": 0 } }, "options": options }));
    assert_eq!(edits.as_array().map(|e| e.len()), Some(1), "{edits}");
    // The project's .dvicode/format.json over the settings; an unsaved buffer.
    std::fs::write(root.join(".dvicode/format.json"), r#"{ "indentSize": 4, "trimTrailingWhitespace": false, "bogus": 1 }"#).unwrap();
    c.open(&uri, 2, "module m;\nlogic a;   \nendmodule\n");
    let edits = c.request_ok("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": options }));
    assert_eq!(edits, json!([{ "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 0 } }, "newText": "    " }]));
    // Turned off.
    std::fs::write(root.join(".dvicode/format.json"), r#"{ "enable": false }"#).unwrap();
    let off = c.request_ok("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": options }));
    assert!(off.is_null(), "{off}");
    c.shutdown();
}

#[test]
fn a_client_without_document_changes_gets_plain_changes() {
    let m = "module m;\n  logic sig;\n  assign sig = 1'b0;\nendmodule\n";
    let root = project("plainedits", &[("m.sv", m)]);
    let (mut c, _) = Client::start_full(&root, Value::Null, Hooks::default(), json!({}));
    let uri = uri(&root, "m.sv");
    let r = c.request_ok("textDocument/rename", json!({ "textDocument": { "uri": uri }, "position": at(m, "sig;"), "newName": "s2" }));
    assert!(r.get("documentChanges").is_none(), "{r}");
    assert_eq!(r["changes"][uri.as_str()].as_array().map(|e| e.len()), Some(2), "{r}");
    c.shutdown();
}

#[test]
fn closing_a_file_outside_the_build_clears_its_problems() {
    let root = project("adhoc", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let scratch = uri(&root, "scratch.sv");
    c.open(&scratch, 7, "module scratch;\n  no_such_type x;\nendmodule\n");
    let p = c.wait_problems(&scratch, |p| !p.diagnostics.is_empty());
    assert_eq!(p.version, Some(7));
    assert_eq!(codes(&p), vec!["NON_EXISTING_TYPE"]);
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": scratch } }));
    let p = c.wait_problems(&scratch, |_| true);
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    assert_eq!(p.version, None);
    c.shutdown();
}

#[test]
fn closing_a_file_outside_the_build_updates_the_files_that_used_it() {
    let top_text = "module top;\n  scratch_t v;\nendmodule\n";
    let root = project("adhocuse", &[("top.sv", top_text)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let top = uri(&root, "top.sv");
    let p = c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["NON_EXISTING_TYPE"]);
    // An open file outside the build declares the type...
    let scratch = uri(&root, "scratch.sv");
    c.open(&scratch, 1, "typedef int scratch_t;\n");
    c.wait_problems(&top, |p| p.diagnostics.is_empty());
    // ... until it is closed.
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": scratch } }));
    let p = c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["NON_EXISTING_TYPE"]);
    c.shutdown();
}

#[test]
fn editing_a_header_updates_the_problems_of_the_file_that_includes_it() {
    // The header opens a function that the including file finishes: renaming a local
    // variable in the header changes no declaration other files see, but breaks the
    // including file, which is not open.
    let h = "  function automatic int f();\n    int k;\n";
    let p_text = "package p;\n`include \"h.svh\"\n    k = 1;\n    return k;\n  endfunction\nendpackage\n";
    let root = project("headerspill", &[("h.svh", h), ("p.sv", p_text)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let (hu, pu) = (uri(&root, "h.svh"), uri(&root, "p.sv"));
    c.open(&hu, 1, h);
    c.change(&hu, 2, &h.replace("int k;", "int k2;"));
    let p = c.wait_problems(&pu, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["UNDECLARED_IDENTIFIER", "UNDECLARED_IDENTIFIER"]);
    assert_eq!(p.diagnostics[0].range.start.line, 2);
    c.change(&hu, 3, h);
    c.wait_problems(&pu, |p| p.diagnostics.is_empty());
    c.shutdown();
}

#[test]
fn problems_of_a_header_go_when_nothing_includes_it_any_more() {
    // Inside a function body, so that what other files see does not change.
    let h = "    int from_h = undeclared_h;\n";
    let a = "module a;\n  function void f();\n`include \"h.svh\"\n  endfunction\nendmodule\n";
    let root = project("dropped", &[("h.svh", h), ("a.sv", a)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let (hu, au) = (uri(&root, "h.svh"), uri(&root, "a.sv"));
    c.wait_problems(&hu, |p| codes(p) == vec!["UNDECLARED_IDENTIFIER"]);
    c.open(&au, 1, a);
    c.change(&au, 2, &a.replace("`include \"h.svh\"\n", ""));
    c.wait_problems(&hu, |p| p.diagnostics.is_empty());
    c.shutdown();
}

#[test]
fn an_open_header_stays_analysed_when_its_last_includer_drops_it() {
    let h = "  int from_h = undeclared_h;\n";
    let a = "module a;\n`include \"h.svh\"\nendmodule\n";
    let root = project("orphan", &[("h.svh", h), ("a.sv", a)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let (hu, au) = (uri(&root, "h.svh"), uri(&root, "a.sv"));
    c.open(&hu, 1, h);
    c.wait_problems(&hu, |p| codes(p) == vec!["UNDECLARED_IDENTIFIER"]);
    c.open(&au, 1, a);
    c.change(&au, 2, "module a;\nendmodule\n");
    // Analysed on its own now (outside a module, so other problems), not dropped.
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": au } }));
    let r = c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": hu } }));
    assert_eq!(r.as_array().map(|v| v.len()), Some(1), "{r}");
    assert_eq!(r[0]["name"], "from_h");
    c.shutdown();
}

#[test]
fn closing_a_header_takes_its_disk_text_back_in_other_open_files() {
    let root = project("closeheader", &[("top.sv", "module top; endmodule\n"), ("h.svh", "  int from_disk;\n")]);
    let (mut c, _) = Client::start(&root, Value::Null);
    // Neither file is part of the build: x.sv's own unit includes the header.
    let (xu, hu) = (uri(&root, "x.sv"), uri(&root, "h.svh"));
    c.open(&xu, 1, "module x;\n`include \"h.svh\"\n  initial from_editor = 1;\nendmodule\n");
    c.wait_problems(&xu, |p| codes(p) == vec!["UNDECLARED_IDENTIFIER"]);
    c.open(&hu, 1, "  int from_editor;\n");
    c.wait_problems(&xu, |p| p.diagnostics.is_empty());
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": hu } }));
    let p = c.wait_problems(&xu, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["UNDECLARED_IDENTIFIER"]);
    c.shutdown();
}

#[test]
fn files_changed_on_disk_update_the_open_files_problems() {
    let root = project("watch", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    // IDLE disappears from the package, which is not open in the editor.
    std::fs::write(root.join("pkg.sv"), PKG.replace("IDLE, BUSY", "READY, BUSY")).unwrap();
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": uri(&root, "pkg.sv"), "type": 2 }] }));
    let p = c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["UNDECLARED_IDENTIFIER"]);
    assert!(p.diagnostics[0].message.contains("IDLE"), "{:?}", p.diagnostics);
    assert_eq!(p.version, Some(1));
    c.shutdown();
}

#[test]
fn rename_edits_every_file_with_the_open_documents_versions() {
    let root = project("rename", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let (pkg, top) = (uri(&root, "pkg.sv"), uri(&root, "top.sv"));
    c.open(&top, 5, TOP);
    let here = json!({ "textDocument": { "uri": top }, "position": at(TOP, "state_e") });
    let range = c.request_ok("textDocument/prepareRename", here.clone());
    assert_eq!(range, json!({ "start": { "line": 2, "character": 2 }, "end": { "line": 2, "character": 9 } }));

    let mut params = here.clone();
    params["newName"] = json!("fsm_e");
    let edit = c.request_ok("textDocument/rename", params);
    let mut docs: Vec<(String, Value, Vec<(u64, u64, String)>)> = edit["documentChanges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let edits = d["edits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| (e["range"]["start"]["line"].as_u64().unwrap(), e["range"]["start"]["character"].as_u64().unwrap(), e["newText"].as_str().unwrap().to_string()))
                .collect();
            (d["textDocument"]["uri"].as_str().unwrap().to_string(), d["textDocument"]["version"].clone(), edits)
        })
        .collect();
    docs.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        docs,
        vec![
            (pkg.to_string(), Value::Null, vec![(1, 30, "fsm_e".to_string())]),
            (top.to_string(), json!(5), vec![(2, 2, "fsm_e".to_string())]),
        ]
    );

    // Not an identifier; not renamable (a keyword position).
    let mut bad = here.clone();
    bad["newName"] = json!("9lives");
    let r = c.request("textDocument/rename", bad);
    assert_eq!(r.error.as_ref().map(|e| e.code), Some(ErrorCode::RequestFailed as i32), "{r:?}");
    let kw = json!({ "textDocument": { "uri": top }, "position": at(TOP, "module") });
    assert_eq!(c.request_ok("textDocument/prepareRename", kw), Value::Null);
    c.shutdown();
}

#[test]
fn a_quick_fix_applied_like_the_editor_does_clears_the_problem() {
    let root = project("quickfix", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let top = uri(&root, "top.sv");
    let broken = TOP.replace("initial s = IDLE;", "initial s = IDEL;").replace('\n', "\r\n");
    c.open(&top, 3, &broken);
    let p = c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    assert_eq!(codes(&p), vec!["UNDECLARED_IDENTIFIER"]);
    let range = p.diagnostics[0].range;
    let actions = c.request_ok(
        "textDocument/codeAction",
        json!({ "textDocument": { "uri": top }, "range": range, "context": { "diagnostics": [], "only": ["quickfix"] } }),
    );
    let fix = actions.as_array().unwrap().iter().find(|a| a["title"] == "Did you mean 'IDLE'?").unwrap_or_else(|| panic!("{actions}")).clone();
    // The editor applies it only to version 3 of the document.
    let docs = fix["edit"]["documentChanges"].as_array().unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0]["textDocument"]["version"], 3);
    let edits = docs[0]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    let (r, new) = (&edits[0]["range"], edits[0]["newText"].as_str().unwrap());
    let li = dvi_syntax::LineIndex::new(&broken);
    let pos = |p: &Value| li.offset(&broken, p["line"].as_u64().unwrap() as u32, p["character"].as_u64().unwrap() as u32) as usize;
    let mut fixed = broken.clone();
    fixed.replace_range(pos(&r["start"])..pos(&r["end"]), new);
    assert_eq!(fixed, TOP.replace('\n', "\r\n"), "only the name changes, line endings kept");
    c.change(&top, 4, &fixed);
    c.wait_problems(&top, |p| p.version == Some(4) && p.diagnostics.is_empty());
    c.shutdown();
}

#[test]
fn bad_requests_get_errors_and_the_server_keeps_answering() {
    let root = project("errors", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let r = c.request("textDocument/noSuchThing", json!({}));
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::MethodNotFound as i32));
    let r = c.request("textDocument/hover", json!({ "textDocument": 3 }));
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::InvalidParams as i32));
    // A panic in a handler: an error reply, a notice for the user, and the next request works.
    let r = c.request("dvicode/debug/fail", json!({}));
    assert_eq!(r.error.map(|e| e.code), Some(ErrorCode::InternalError as i32));
    let e = c.wait_note("dvicode/internalError", |_| true);
    assert_eq!(e["feature"], "dvicode/debug/fail");
    assert!(e["location"].as_str().unwrap().contains("server.rs"), "{e}");
    // Positions in files the server does not know answer nothing rather than fail.
    let r = c.request_ok("textDocument/hover", json!({ "textDocument": { "uri": uri(&root, "nowhere.sv") }, "position": { "line": 0, "character": 0 } }));
    assert_eq!(r, Value::Null);
    assert_eq!(c.request_ok("dvicode/designTops", Value::Null), json!(["top"]));
    c.shutdown();
}

#[test]
fn remote_file_requests_carry_error_codes() {
    let root = project("fs", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let path = root.join("new dir").join("ü.sv");
    let dir = root.join("new dir");
    let s = |p: &Path| p.to_str().unwrap().to_string();
    c.request_ok("dvicode/fs/createDirectory", json!({ "path": s(&dir) }));
    let w = c.request_ok("dvicode/fs/writeFile", json!({ "path": s(&path), "content": "bW9kdWxlIG07IGVuZG1vZHVsZQo=", "create": true, "overwrite": false }));
    assert_eq!(w["created"], true, "{w}");
    let r = c.request_ok("dvicode/fs/readFile", json!({ "path": s(&path) }));
    assert_eq!(r["content"], "bW9kdWxlIG07IGVuZG1vZHVsZQo=");
    let entries = c.request_ok("dvicode/fs/readDirectory", json!({ "path": s(&dir) }));
    assert_eq!(entries, json!([["ü.sv", 1]]));
    let r = c.request("dvicode/fs/writeFile", json!({ "path": s(&path), "content": "", "create": true, "overwrite": false }));
    assert_eq!(r.error.and_then(|e| e.data), Some(json!({ "code": "FileExists" })));
    let r = c.request("dvicode/fs/stat", json!({ "path": s(&root.join("missing.sv")) }));
    assert_eq!(r.error.and_then(|e| e.data), Some(json!({ "code": "FileNotFound" })));
    c.shutdown();
}

/// `dvicode/fs/*` as the client's file system provider uses it: VS Code's semantics for
/// create, overwrite, rename onto an existing entry and recursive delete.
#[test]
fn remote_file_writes_follow_the_editor_file_system_semantics() {
    use base64::Engine;
    let root = project("fswrite", &[("top.sv", TOP)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let d = root.join("fs");
    std::fs::create_dir_all(&d).unwrap();
    let s = |p: &Path| p.to_str().unwrap().to_string();
    let b64 = |t: &str| base64::engine::general_purpose::STANDARD.encode(t);
    let code = |r: Response| r.error.and_then(|e| e.data).and_then(|d| d["code"].as_str().map(str::to_string)).unwrap_or_else(|| "ok".into());
    let write = |c: &mut Client, p: &Path, text: &str, create: bool, overwrite: bool| {
        code(c.request("dvicode/fs/writeFile", json!({ "path": s(p), "content": b64(text), "create": create, "overwrite": overwrite })))
    };
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();

    // writeFile: create, overwrite, and the refusals.
    let a = d.join("a.sv");
    assert_eq!(write(&mut c, &a, "one\r\n", false, true), "FileNotFound");
    assert!(!a.exists());
    assert_eq!(write(&mut c, &a, "one\r\n", true, false), "ok");
    assert_eq!(read(&a), "one\r\n", "bytes as sent, line endings included");
    assert_eq!(write(&mut c, &a, "two\n", true, false), "FileExists");
    assert_eq!(read(&a), "one\r\n");
    assert_eq!(write(&mut c, &a, "two\n", false, true), "ok");
    assert_eq!(read(&a), "two\n");
    assert_eq!(write(&mut c, &a, "", true, true), "ok");
    assert_eq!(read(&a), "", "an empty file");
    assert_eq!(write(&mut c, &d.join("missing/b.sv"), "x", true, true), "FileNotFound");
    assert_eq!(write(&mut c, &d, "x", true, true), "FileIsADirectory");
    assert!(d.is_dir());

    // createDirectory.
    let sub = d.join("sub");
    assert_eq!(code(c.request("dvicode/fs/createDirectory", json!({ "path": s(&sub) }))), "ok");
    assert_eq!(code(c.request("dvicode/fs/createDirectory", json!({ "path": s(&sub) }))), "FileExists");
    assert_eq!(code(c.request("dvicode/fs/createDirectory", json!({ "path": s(&d.join("no/such")) }))), "FileNotFound");

    // rename: onto an existing file only with overwrite; a directory onto a directory.
    let b = d.join("b.sv");
    std::fs::write(&a, "from a").unwrap();
    std::fs::write(&b, "from b").unwrap();
    let rename = |c: &mut Client, from: &Path, to: &Path, overwrite: bool| code(c.request("dvicode/fs/rename", json!({ "from": s(from), "to": s(to), "overwrite": overwrite })));
    assert_eq!(rename(&mut c, &a, &b, false), "FileExists");
    assert_eq!((read(&a).as_str(), read(&b).as_str()), ("from a", "from b"));
    assert_eq!(rename(&mut c, &a, &b, true), "ok");
    assert!(!a.exists());
    assert_eq!(read(&b), "from a");
    assert_eq!(rename(&mut c, &a, &b, true), "FileNotFound");
    let other = d.join("other");
    std::fs::create_dir_all(other.join("deep")).unwrap();
    std::fs::write(other.join("deep/old.sv"), "old").unwrap();
    std::fs::write(sub.join("new.sv"), "new").unwrap();
    assert_eq!(rename(&mut c, &sub, &other, false), "FileExists");
    assert_eq!(rename(&mut c, &sub, &other, true), "ok");
    assert!(!sub.exists());
    assert_eq!(read(&other.join("new.sv")), "new");
    assert!(!other.join("deep").exists(), "the replaced directory is gone");
    // A file onto a directory and back.
    assert_eq!(rename(&mut c, &b, &other, true), "ok");
    assert_eq!(read(&other), "from a");
    std::fs::create_dir_all(&sub).unwrap();
    assert_eq!(rename(&mut c, &sub, &other, true), "ok");
    assert!(other.is_dir());

    // delete: recursive only when asked; missing entries.
    let tree = d.join("tree");
    std::fs::create_dir_all(tree.join("x/y")).unwrap();
    std::fs::write(tree.join("x/y/z.sv"), "z").unwrap();
    let delete = |c: &mut Client, p: &Path, recursive: bool| code(c.request("dvicode/fs/delete", json!({ "path": s(p), "recursive": recursive })));
    assert_ne!(delete(&mut c, &tree, false), "ok");
    assert!(tree.join("x/y/z.sv").exists());
    assert_eq!(delete(&mut c, &tree, true), "ok");
    assert!(!tree.exists());
    assert_eq!(delete(&mut c, &tree, true), "FileNotFound");
    assert_eq!(delete(&mut c, &other, false), "ok", "an empty directory");
    #[cfg(unix)]
    {
        // A link to a directory is deleted, not what it points to.
        let target = d.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.sv"), "keep").unwrap();
        let link = d.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(delete(&mut c, &link, false), "ok");
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert_eq!(read(&target.join("keep.sv")), "keep");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(delete(&mut c, &link, true), "ok");
        assert_eq!(read(&target.join("keep.sv")), "keep");
    }
    // The server keeps answering.
    assert_eq!(c.request_ok("dvicode/status", Value::Null)["top_files"], 1);
    c.shutdown();
}

#[test]
fn a_new_build_file_setting_rebuilds() {
    let root = project("settings", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    std::fs::write(root.join(".dvicode/pkg_only.build"), "pkg.sv\n").unwrap();
    let (mut c, _) = Client::start(&root, json!({ "buildFile": ".dvicode/pkg_only.build" }));
    assert_eq!(c.request_ok("dvicode/status", Value::Null)["top_files"], 1);
    // The same settings again (the client sends them at start-up) change nothing; new ones rebuild.
    c.notify("workspace/didChangeConfiguration", json!({ "settings": { "dvicode": { "buildFile": ".dvicode/pkg_only.build" } } }));
    c.notify("workspace/didChangeConfiguration", json!({ "settings": { "dvicode": { "buildFile": "" } } }));
    let st = c.wait_note("dvicode/status", |_| true);
    assert_eq!(st["top_files"], 2, "{st}");
    assert!(st["build_file"].as_str().unwrap().ends_with("default.build"), "{st}");
    c.shutdown();
}

#[test]
fn a_setting_of_the_wrong_type_keeps_the_others() {
    let s: super::Settings = serde_json::from_value(json!({ "maxProblems": -1, "buildFile": "a.build", "uvmHome": 3, "env": { "A": "1" } })).unwrap();
    assert_eq!((s.build_file.as_deref(), s.max_problems, s.uvm_home), (Some("a.build"), None, None));
    assert_eq!(s.env.get("A").map(String::as_str), Some("1"));
}

#[test]
fn type_hierarchy_walks_bases_and_subclasses() {
    let text = "package p;\n  class base_c; endclass\n  class mid_c extends base_c; endclass\n  class leaf_c extends mid_c; endclass\nendpackage\n";
    let root = project("typeh", &[("p.sv", text)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let u = uri(&root, "p.sv");
    let items = c.request_ok("textDocument/prepareTypeHierarchy", json!({ "textDocument": { "uri": u }, "position": at(text, "mid_c extends") }));
    assert_eq!(items[0]["name"], "mid_c", "{items}");
    let sup = c.request_ok("typeHierarchy/supertypes", json!({ "item": items[0] }));
    let sub = c.request_ok("typeHierarchy/subtypes", json!({ "item": items[0] }));
    assert_eq!((sup[0]["name"].as_str(), sub[0]["name"].as_str()), (Some("base_c"), Some("leaf_c")), "{sup} {sub}");
    c.shutdown();
}

/// Holds the server's update thread while it is closed, so that a test can talk to the server
/// while an update runs (as a header that thousands of files include keeps it busy).
#[derive(Default)]
struct Gate {
    /// Closed, and how many may pass anyway (`allow`).
    closed: Mutex<(bool, usize)>,
    opened: Condvar,
    /// Updates that reached the gate.
    entered: AtomicUsize,
}

impl Gate {
    /// Hooks that hold updates.
    fn hooks(self: &Arc<Self>) -> Hooks {
        let g = self.clone();
        Hooks { before_update: Some(Box::new(move || g.pass())), ..Default::default() }
    }

    /// Hooks that hold the handlers of requests `method`.
    fn request_hooks(self: &Arc<Self>, method: &'static str) -> Hooks {
        let g = self.clone();
        Hooks { before_request: Some(Box::new(move |m| if m == method { g.pass() })), ..Default::default() }
    }

    fn pass(&self) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut closed = self.closed.lock().unwrap();
        loop {
            match *closed {
                (false, _) => return,
                (true, n) if n > 0 => {
                    closed.1 -= 1;
                    return;
                }
                _ => closed = self.opened.wait(closed).unwrap(),
            }
        }
    }

    fn close(&self) {
        *self.closed.lock().unwrap() = (true, 0);
    }

    fn open(&self) {
        *self.closed.lock().unwrap() = (false, 0);
        self.opened.notify_all();
    }

    /// Lets `n` more pass while it stays closed.
    fn allow(&self, n: usize) {
        self.closed.lock().unwrap().1 += n;
        self.opened.notify_all();
    }

    /// Waits until `n` updates in all have reached the gate.
    fn wait_entered(&self, n: usize) {
        let deadline = Instant::now() + WAIT;
        while self.entered.load(Ordering::SeqCst) < n {
            assert!(Instant::now() < deadline, "{} update(s) reached the gate, not {n}", self.entered.load(Ordering::SeqCst));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Opens the gate when a test ends, also when it fails, so that the update thread finishes.
struct Opener(Arc<Gate>);

impl Drop for Opener {
    fn drop(&mut self) {
        self.0.open();
    }
}

const FRESH: &str = "module top;\n  import p::*;\n  int fresh;\n  state_e s;\n  initial fresh = 2;\nendmodule\n";

/// The definition of `fresh` in top.sv, as `FRESH` declares it.
fn fresh_definition(c: &mut Client, top: &Url, text: &str) -> RequestId {
    c.send_request("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(text, "fresh = 2") }))
}

#[test]
fn a_request_is_answered_while_a_slow_update_runs() {
    let root = project("slowupdate", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (pkg, top) = (uri(&root, "pkg.sv"), uri(&root, "top.sv"));
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // The next update stops at the gate.
    gate.close();
    c.change(&top, 2, FRESH);
    // Asked right after the edit: waits for the update that analyses it, which starts now.
    let waiting = fresh_definition(&mut c, &top, FRESH);
    gate.wait_entered(2);
    // Meanwhile the server reads and answers requests about files the update leaves alone.
    let h = c.request_ok("textDocument/hover", json!({ "textDocument": { "uri": pkg }, "position": at(PKG, "twice") }));
    assert!(h["contents"]["value"].as_str().unwrap_or("").contains("twice"), "{h}");
    assert_eq!(c.request_ok("dvicode/serverInfo", Value::Null)["pid"], std::process::id());
    assert!(c.reply(&waiting, Duration::from_millis(300)).is_none(), "answered before its edit was analysed");
    // Once the update is in, the waiting request sees the new declaration.
    gate.open();
    let r = c.reply(&waiting, WAIT).expect("a reply once the update is in");
    let locs = r.result.unwrap_or(Value::Null);
    assert_eq!(locs.as_array().map(|l| l.len()), Some(1), "{locs}");
    assert_eq!(locs[0]["range"]["start"]["line"], 2);
    c.shutdown();
}

const MACRO_DEFS: &str = "// no macros yet\n";

const MACRO_USER: &str = "module body;\n`ifdef WIDE\n  int wide_x;\n`endif\n  initial wide_x = 1;\nendmodule\n";

#[test]
fn a_request_waits_for_an_edit_of_the_macros_an_earlier_file_defines() {
    // Two units of one invocation: body.sv is compiled with the macros defs.sv leaves.
    let root = project("earliermacros", &[("defs.sv", MACRO_DEFS), ("body.sv", MACRO_USER)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (defs, body) = (uri(&root, "defs.sv"), uri(&root, "body.sv"));
    c.open(&defs, 1, MACRO_DEFS);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    c.open(&body, 1, MACRO_USER);
    gate.wait_entered(2);
    c.wait_note("dvicode/revision", |_| true);
    // An edit of body.sv, whose update is held at the gate.
    gate.close();
    let v2 = format!("{MACRO_USER}// edited\n");
    c.change(&body, 2, &v2);
    let first = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": body } }));
    gate.wait_entered(3);
    // A request about defs.sv does not wait for it: macros do not flow back.
    assert_eq!(c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": defs } })), json!([]));
    // Then defs.sv defines the macro that declares `wide_x` in body.sv.
    c.change(&defs, 2, "`define WIDE\n");
    let waiting = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": body }, "position": at(&v2, "wide_x = 1") }));
    // The server has it before body.sv's update goes on.
    c.sync();
    gate.open();
    assert!(c.reply(&first, WAIT).is_some_and(|r| r.error.is_none()));
    // Not answered when body.sv's update is in, but once defs.sv's macros reached it.
    let r = c.reply(&waiting, WAIT).expect("a reply once the updates are in");
    let locs = r.result.unwrap_or(Value::Null);
    assert_eq!(locs.as_array().map(|l| l.len()), Some(1), "{locs}");
    assert_eq!(locs[0]["range"]["start"]["line"], 2);
    c.shutdown();
}

#[test]
fn a_newer_edit_replaces_the_update_in_progress() {
    let root = project("supersede", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    let first = c.wait_note("dvicode/revision", |_| true)["revision"].as_u64().unwrap();
    gate.close();
    // An edit whose update is held at the gate, then another of the same file.
    c.change(&top, 2, &TOP.replace("initial s = IDLE;", "initial s = IDLE;\n  initial undeclared_a = 1;"));
    c.request_ok("dvicode/serverInfo", Value::Null);
    let symbols = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(2);
    let v3 = FRESH.replace("initial fresh = 2;", "initial fresh = 2;\n  initial undeclared_b = 1;");
    c.change(&top, 3, &v3);
    let waiting = fresh_definition(&mut c, &top, &v3);
    // Once the server has read the edit and replaced the held update, which stops as soon
    // as it goes on; the one that replaces it comes next.
    c.sync();
    gate.open();
    gate.wait_entered(3);
    // One update, of the newest text, is installed: the replaced one is not.
    let r = c.reply(&waiting, WAIT).expect("a reply once the update is in");
    assert_eq!(r.result.unwrap_or(Value::Null)[0]["range"]["start"]["line"], 2);
    assert!(c.reply(&symbols, WAIT).is_some_and(|r| r.error.is_none()));
    let next = c.wait_note("dvicode/revision", |_| true)["revision"].as_u64().unwrap();
    assert_eq!(next, first + 1);
    let p = c.wait_problems(&top, |p| p.version == Some(3));
    assert!(p.diagnostics.len() == 1 && p.diagnostics[0].message.contains("undeclared_b"), "{:?}", p.diagnostics);
    c.shutdown();
}

#[test]
fn shutdown_is_answered_while_an_update_runs() {
    let root = project("shutdownbusy", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    gate.close();
    c.open(&uri(&root, "top.sv"), 1, TOP);
    gate.wait_entered(1);
    c.shutdown();
}

fn cancel(c: &Client, id: &RequestId) {
    c.notify("$/cancelRequest", json!({ "id": serde_json::to_value(id).unwrap() }));
}

fn error_code(r: &Response) -> Option<i32> {
    r.error.as_ref().map(|e| e.code)
}

#[test]
fn a_cancelled_request_gets_a_cancellation_error() {
    let root = project("cancelwaiting", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    gate.close();
    c.change(&top, 2, FRESH);
    let waiting = fresh_definition(&mut c, &top, FRESH);
    gate.wait_entered(2);
    // Cancelled while it waits for the update: answered at once, the update still held.
    cancel(&c, &waiting);
    let r = c.reply(&waiting, WAIT).expect("a reply to the cancelled request");
    assert_eq!(error_code(&r), Some(ErrorCode::RequestCanceled as i32), "{r:?}");
    gate.open();
    // Once only; later requests are answered as usual.
    let r = c.request_ok("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(FRESH, "fresh = 2") }));
    assert_eq!(r[0]["range"]["start"]["line"], 2, "{r}");
    assert!(c.reply(&waiting, Duration::from_millis(200)).is_none(), "a second reply");
    c.shutdown();
}

#[test]
fn a_request_cancelled_while_its_handler_runs_gets_a_cancellation_error() {
    let root = project("cancelrunning", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.request_hooks("textDocument/references"));
    let pkg = uri(&root, "pkg.sv");
    let params = json!({ "textDocument": { "uri": pkg }, "position": at(PKG, "twice"), "context": { "includeDeclaration": true } });
    gate.close();
    let id = c.send_request("textDocument/references", params.clone());
    gate.wait_entered(1);
    cancel(&c, &id);
    // Cancelling an answered or unknown request changes nothing.
    cancel(&c, &RequestId::from(9999));
    c.sync();
    gate.open();
    let r = c.reply(&id, WAIT).expect("a reply to the cancelled request");
    assert_eq!(error_code(&r), Some(ErrorCode::RequestCanceled as i32), "{r:?}");
    let refs = c.request_ok("textDocument/references", params);
    assert_eq!(refs.as_array().map(|r| r.len()), Some(2), "{refs}");
    c.shutdown();
}

#[test]
fn a_waiting_request_about_a_position_is_dropped_when_its_document_changes() {
    let root = project("contentmodified", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    gate.close();
    c.change(&top, 2, FRESH);
    let at_position = fresh_definition(&mut c, &top, FRESH);
    let symbols = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(2);
    // Its position is in text that is gone; a request about the whole document goes on, and
    // is answered from the text it was sent after.
    c.change(&top, 3, &FRESH.replace("  int fresh;\n", "  int fresh;\n  int more;\n"));
    let r = c.reply(&at_position, WAIT).expect("a reply to the dropped request");
    assert_eq!(error_code(&r), Some(ErrorCode::ContentModified as i32), "{r:?}");
    assert!(c.reply(&symbols, Duration::from_millis(200)).is_none());
    gate.open();
    let r = c.reply(&symbols, WAIT).expect("symbols once the update is in");
    let names: Vec<&str> = r.result.as_ref().and_then(|v| v[0]["children"].as_array()).into_iter().flatten().filter_map(|s| s["name"].as_str()).collect();
    assert!(names.contains(&"fresh") && !names.contains(&"more"), "{names:?}");
    c.shutdown();
}

#[test]
fn a_problem_check_replaced_by_a_newer_one_stops_computing() {
    let pkg = "package p;\n  typedef int t0;\nendpackage\n";
    let user = |m: &str| format!("module {m};\n  import p::*;\n  initial x_{m} = 1;\nendmodule\n");
    let (a, b, c3) = (user("a"), user("b"), user("c"));
    let root = project("problemjobs", &[("pkg.sv", pkg), ("a.sv", &a), ("b.sv", &b), ("c.sv", &c3)]);
    // Every file a check looks at: (check, file name, computed). The gate holds computations
    // (on a worker thread: nothing below needs another one until it opens).
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let seen: Arc<Mutex<Vec<(u64, String, bool)>>> = Arc::default();
    let hooks = {
        let (g, seen) = (gate.clone(), seen.clone());
        Hooks {
            before_problems: Some(Box::new(move |job, path, computed| {
                seen.lock().unwrap().push((job, path.file_name().unwrap().to_string_lossy().to_string(), computed));
                if computed {
                    g.pass();
                }
            })),
            ..Default::default()
        }
    };
    // The first build's check of every file is held at its first file.
    gate.close();
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    gate.wait_entered(1);
    // Closing a file outside the build that was open starts a check of every file.
    let scratch = uri(&root, "scratch.sv");
    c.open(&scratch, 1, "module scratch;\nendmodule\n");
    c.wait_note("dvicode/revision", |_| true);
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": scratch } }));
    c.sync();
    gate.open();
    let files = ["pkg.sv", "a.sv", "b.sv", "c.sv"];
    let job = |s: &[(u64, String, bool)], f: &str, computed: bool| s.iter().find(|x| x.1 == f && x.2 == computed).map(|x| x.0);
    let looked_at = |j: u64| seen.lock().unwrap().iter().filter(|s| s.0 == j && files.contains(&s.1.as_str())).map(|s| (s.1.clone(), s.2)).collect::<Vec<_>>();
    let deadline = Instant::now() + WAIT;
    let (older, newer) = loop {
        let s = seen.lock().unwrap().clone();
        let older = s[0].0;
        if let Some(newer) = job(&s, "c.sv", true).filter(|n| *n != older) {
            if looked_at(older).len() == 4 && looked_at(newer).len() == 4 {
                break (older, newer);
            }
        }
        assert!(Instant::now() < deadline, "{s:?}");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(older < newer);
    // The older check computed the file it was held at, and no other.
    let all = |v: bool| files.iter().map(|f| (f.to_string(), v)).collect::<Vec<_>>();
    let mut expected = all(false);
    expected[0].1 = true;
    assert_eq!(looked_at(older), expected);
    assert_eq!(looked_at(newer), all(true));
    c.wait_problems(&uri(&root, "c.sv"), |p| !p.diagnostics.is_empty());
    c.shutdown();
}

#[test]
fn edits_analysed_during_a_build_stay_in_use_until_its_replay_is_done() {
    let old = "package p;\n  class Old; endclass\nendpackage\n";
    let new = old.replace("Old", "New");
    let top_text = "module top;\n  import p::*;\n  New n;\nendmodule\n";
    let root = project("buildreplay", &[("pkg.sv", old), ("top.sv", top_text)]);
    let (builds, updates) = (Arc::new(Gate::default()), Arc::new(Gate::default()));
    let _openers = (Opener(builds.clone()), Opener(updates.clone()));
    let hooks = {
        let (b, u) = (builds.clone(), updates.clone());
        Hooks { after_build: Some(Box::new(move || b.pass())), before_update: Some(Box::new(move || u.pass())), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let (pkg, top) = (uri(&root, "pkg.sv"), uri(&root, "top.sv"));
    let definition = |c: &mut Client| c.request_ok("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(top_text, "New n") }));
    c.open(&pkg, 1, old);
    updates.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // A full build that has read the class as Old waits to be handed over.
    builds.close();
    c.request_ok("dvicode/rebuild", Value::Null);
    builds.wait_entered(2);
    // Renamed meanwhile, and analysed: top.sv finds New.
    c.change(&pkg, 2, &new);
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": pkg } }));
    assert_eq!(definition(&mut c)[0]["uri"], pkg.as_str());
    // The build is done; the rename is applied to it again, held at the gate. Until then
    // requests use the snapshot that has the rename, not the build's older text.
    updates.close();
    builds.open();
    updates.wait_entered(3);
    assert_eq!(definition(&mut c)[0]["uri"], pkg.as_str());
    updates.open();
    c.wait_note("dvicode/status", |_| true);
    assert_eq!(definition(&mut c)[0]["uri"], pkg.as_str());
    c.shutdown();
}

#[test]
fn a_file_changed_on_disk_while_an_update_reads_it_is_read_again() {
    let top_text = "module top;\nendmodule\n";
    let root = project("watchupdate", &[("top.sv", top_text), ("h.svh", "  typedef int old_t;\n")]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let hooks = {
        let g = gate.clone();
        Hooks { after_update: Some(Box::new(move || g.pass())), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let top = uri(&root, "top.sv");
    c.open(&top, 1, top_text);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // The edit includes h.svh, which no snapshot compiles yet; its update has read it and is
    // held before it is installed when the header changes on disk.
    gate.close();
    c.change(&top, 2, "module top;\n`include \"h.svh\"\n  new_t v;\nendmodule\n");
    let symbols = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(2);
    std::fs::write(root.join("h.svh"), "  typedef int new_t;\n").unwrap();
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": uri(&root, "h.svh"), "type": 2 }] }));
    // A request about the whole project, sent before the update is in, waits for the header
    // to be read again.
    let found = c.send_request("workspace/symbol", json!({ "query": "new_t" }));
    c.sync();
    gate.open();
    c.reply(&symbols, WAIT).expect("symbols once the update is in");
    let found = c.reply(&found, WAIT).expect("symbols of the project").result.unwrap_or(Value::Null);
    let found: Vec<&str> = found.as_array().into_iter().flatten().filter_map(|s| s["location"]["uri"].as_str()).collect();
    assert!(found.len() == 1 && found[0].ends_with("h.svh"), "{found:?}");
    c.shutdown();
}

#[test]
fn a_new_build_file_is_announced_when_its_build_is_in_use() {
    let root = project("announce", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    std::fs::write(root.join(".dvicode/pkg_only.build"), "pkg.sv\n").unwrap();
    let (builds, updates) = (Arc::new(Gate::default()), Arc::new(Gate::default()));
    let _openers = (Opener(builds.clone()), Opener(updates.clone()));
    let hooks = {
        let (b, u) = (builds.clone(), updates.clone());
        Hooks { after_build: Some(Box::new(move || b.pass())), before_update: Some(Box::new(move || u.pass())), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, json!({ "buildFile": ".dvicode/pkg_only.build" }), hooks);
    let pkg = uri(&root, "pkg.sv");
    c.open(&pkg, 1, PKG);
    updates.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // Another build file; the package is edited while its build runs.
    builds.close();
    c.notify("workspace/didChangeConfiguration", json!({ "settings": { "dvicode": { "buildFile": "" } } }));
    builds.wait_entered(2);
    c.change(&pkg, 2, &PKG.replace("BUSY", "BUSY, DONE"));
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": pkg } }));
    // The build is done, the edit is applied to it again (held): not in use yet, not announced.
    updates.close();
    builds.open();
    updates.wait_entered(3);
    c.sync();
    assert!(!c.notes.iter().any(|n| n.method == "window/showMessage"), "announced before it is in use");
    assert_eq!(c.request_ok("dvicode/serverInfo", Value::Null)["pid"], std::process::id());
    updates.open();
    let m = c.wait_note("window/showMessage", |_| true);
    assert!(m["message"].as_str().unwrap_or("").contains("default.build"), "{m}");
    assert_eq!(c.request_ok("dvicode/status", Value::Null)["top_files"], 2);
    c.shutdown();
}

/// Counts the problem checks not finished yet, so that a test can wait for all of them.
#[derive(Default)]
struct Checks(AtomicIsize);

impl Checks {
    fn hooks(self: &Arc<Self>) -> Hooks {
        Hooks { problem_check: Some(self.hook()), ..Default::default() }
    }

    fn hook(self: &Arc<Self>) -> Box<dyn Fn(bool) + Send + Sync> {
        let c = self.clone();
        Box::new(move |started| {
            c.0.fetch_add(if started { 1 } else { -1 }, Ordering::SeqCst);
        })
    }

    /// Once the server has read what was sent, waits until every check it started is done, and
    /// receives what they published.
    fn wait_done(&self, c: &mut Client) {
        c.sync();
        let deadline = Instant::now() + WAIT;
        while self.0.load(Ordering::SeqCst) != 0 {
            assert!(Instant::now() < deadline, "problem checks still running");
            std::thread::sleep(Duration::from_millis(5));
        }
        c.sync();
    }
}

#[test]
fn a_request_about_the_project_waits_for_earlier_edits_of_other_files() {
    let root = project("projectwait", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (pkg, top) = (uri(&root, "pkg.sv"), uri(&root, "top.sv"));
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // A second call of twice() in top.sv, held at the gate; references asked from pkg.sv.
    gate.close();
    c.change(&top, 2, &TOP.replace("int n = twice(3);", "int n = twice(3);\n  int m = twice(4);"));
    let params = json!({ "textDocument": { "uri": pkg }, "position": at(PKG, "twice"), "context": { "includeDeclaration": true } });
    let refs = c.send_request("textDocument/references", params);
    gate.wait_entered(2);
    // A request about pkg.sv alone does not wait.
    c.request_ok("textDocument/hover", json!({ "textDocument": { "uri": pkg }, "position": at(PKG, "twice") }));
    assert!(c.reply(&refs, Duration::from_millis(200)).is_none(), "answered without the edit of top.sv");
    gate.open();
    let r = c.reply(&refs, WAIT).expect("references once the edit is in");
    assert_eq!(r.result.as_ref().and_then(|v| v.as_array()).map(|v| v.len()), Some(3), "{r:?}");
    c.shutdown();
}

#[test]
fn a_request_about_a_file_waits_for_earlier_edits_of_the_headers_it_includes() {
    let h = "  function automatic int f();\n    int k;\n";
    let p_text = "package p;\n`include \"h.svh\"\n    k = 1;\n    return k;\n  endfunction\nendpackage\n";
    let root = project("headerwait", &[("h.svh", h), ("p.sv", p_text)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (hu, pu) = (uri(&root, "h.svh"), uri(&root, "p.sv"));
    c.open(&hu, 1, h);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // k is renamed in the header; where k in p.sv is declared asks for that edit.
    gate.close();
    c.change(&hu, 2, &h.replace("int k;", "int k2;"));
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": pu }, "position": at(p_text, "k = 1") }));
    gate.wait_entered(2);
    assert!(c.reply(&def, Duration::from_millis(200)).is_none(), "answered without the edit of the header");
    gate.open();
    let r = c.reply(&def, WAIT).expect("an answer once the edit is in");
    assert_eq!(r.result.as_ref().and_then(|v| v.as_array()).map(|v| v.len()), Some(0), "{r:?}");
    c.shutdown();
}

#[test]
fn typing_in_another_file_does_not_hold_up_a_waiting_request() {
    let other = "module other;\nendmodule\n";
    let root = project("starve", &[("pkg.sv", PKG), ("top.sv", TOP), ("other.sv", other)]);
    // Updates as slow as typing is fast (a large project), once the files are open.
    let slow = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hooks = {
        let slow = slow.clone();
        Hooks {
            before_update: Some(Box::new(move || {
                if slow.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(150));
                }
            })),
            ..Default::default()
        }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let (pkg, ou) = (uri(&root, "pkg.sv"), uri(&root, "other.sv"));
    for u in [&pkg, &ou] {
        let text = std::fs::read_to_string(u.to_file_path().unwrap()).unwrap();
        c.open(u, 1, &text);
        c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": u } }));
    }
    slow.store(true, Ordering::SeqCst);
    // other.sv and pkg.sv edited, then a question about pkg.sv: one update of both starts.
    let mut v = 2;
    c.change(&ou, v, &format!("module other;\n  int x{v};\nendmodule\n"));
    let pkg2 = PKG.replace("BUSY", "BUSY, DONE");
    c.change(&pkg, 2, &pkg2);
    let waiting = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": pkg }, "position": at(&pkg2, "DONE") }));
    // Typing goes on in other.sv, faster than an update, and the editor asks about it after
    // each key: none of that replaces the update the question waits for.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        v += 1;
        c.change(&ou, v, &format!("module other;\n  int x{v};\nendmodule\n"));
        let _ = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": ou } }));
        if let Some(r) = c.reply(&waiting, Duration::from_millis(30)) {
            assert_eq!(r.result.as_ref().and_then(|v| v.as_array()).map(|v| v.len()), Some(1), "{r:?}");
            slow.store(false, Ordering::SeqCst);
            c.shutdown();
            return;
        }
    }
    panic!("no answer while other.sv was typed in");
}

#[test]
fn a_waiting_request_does_not_wait_for_a_pause_in_typing_elsewhere() {
    let other = "module other;\nendmodule\n";
    let root = project("pause", &[("pkg.sv", PKG), ("top.sv", TOP), ("other.sv", other)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (pkg, top, ou) = (uri(&root, "pkg.sv"), uri(&root, "top.sv"), uri(&root, "other.sv"));
    for u in [&pkg, &top, &ou] {
        let text = std::fs::read_to_string(u.to_file_path().unwrap()).unwrap();
        c.open(u, 1, &text);
        c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": u } }));
    }
    // An update of top.sv is held; pkg.sv is edited and asked about meanwhile.
    gate.close();
    // Counted before the edit, whose update may start with the pause in typing.
    let n = gate.entered.load(Ordering::SeqCst);
    c.change(&top, 2, FRESH);
    let _ = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(n + 1);
    let pkg2 = PKG.replace("BUSY", "BUSY, DONE");
    c.change(&pkg, 2, &pkg2);
    let waiting = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": pkg }, "position": at(&pkg2, "DONE") }));
    c.sync();
    gate.open();
    // Typing goes on in other.sv, faster than the pause that starts an update.
    for v in 2..60 {
        c.change(&ou, v, &format!("module other;\n  int x{v};\nendmodule\n"));
        if let Some(r) = c.reply(&waiting, Duration::from_millis(50)) {
            assert_eq!(r.result.as_ref().and_then(|v| v.as_array()).map(|v| v.len()), Some(1), "{r:?}");
            c.shutdown();
            return;
        }
    }
    panic!("no answer while other.sv was typed in");
}

#[test]
fn a_waiting_hierarchy_request_is_dropped_when_its_document_changes() {
    let text = "package p;\n  class base_c; endclass\n  class mid_c extends base_c; endclass\nendpackage\n";
    let root = project("hiermodified", &[("p.sv", text)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let u = uri(&root, "p.sv");
    c.open(&u, 1, text);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    let items = c.request_ok("textDocument/prepareTypeHierarchy", json!({ "textDocument": { "uri": u }, "position": at(text, "mid_c extends") }));
    gate.close();
    let edited = text.replace("package p;\n", "package p;\n  // moved down\n");
    c.change(&u, 2, &edited);
    let sup = c.send_request("typeHierarchy/supertypes", json!({ "item": items[0] }));
    gate.wait_entered(2);
    // The item's range is in text that is gone.
    c.change(&u, 3, &edited.replace("moved", "moved again"));
    let r = c.reply(&sup, WAIT).expect("a reply to the dropped request");
    assert_eq!(error_code(&r), Some(ErrorCode::ContentModified as i32), "{r:?}");
    gate.open();
    c.shutdown();
}

#[test]
fn exit_stops_a_running_update_and_lets_go_of_the_connection() {
    let root = project("exitbusy", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let finished = Arc::new(AtomicUsize::new(0));
    let hooks = {
        let (g, f) = (gate.clone(), finished.clone());
        Hooks {
            before_update: Some(Box::new(move || g.pass())),
            after_update: Some(Box::new(move || {
                f.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    gate.close();
    c.open(&uri(&root, "top.sv"), 1, TOP);
    gate.wait_entered(1);
    c.request_ok("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    c.server.take().unwrap().join().expect("server thread");
    // Let go, the held update stops at once: it is not finished, and the server's side of the
    // connection is closed (nothing holds a sender any more).
    gate.open();
    let deadline = Instant::now() + WAIT;
    loop {
        match c.conn.receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(_) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Err(e) => panic!("the connection stays open: {e:?}"),
        }
    }
    assert_eq!(finished.load(Ordering::SeqCst), 0, "the update went on after exit");
}

#[test]
fn problems_carry_the_version_of_the_text_they_were_found_in() {
    let root = project("versions", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let checks = Arc::new(Checks::default());
    let hooks = {
        let g = gate.clone();
        Hooks { before_update: Some(Box::new(move || g.pass())), problem_check: Some(checks.hook()), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // An update of version 2 is held while version 3 comes; whether it is installed or
    // replaced, each publication names the version whose text it was found in.
    gate.close();
    let text = |v: usize| TOP.replace("initial s = IDLE;", &format!("initial s = IDLE;\n  initial undeclared_{v} = 1;"));
    c.change(&top, 2, &text(2));
    let _ = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(2);
    c.change(&top, 3, &text(3));
    c.sync();
    gate.open();
    c.wait_problems(&top, |p| p.version == Some(3));
    checks.wait_done(&mut c);
    for p in c.published.iter().filter(|p| p.uri == top) {
        let v = p.version.unwrap_or_else(|| panic!("a publication without a version: {p:?}"));
        assert!(p.diagnostics.iter().all(|d| d.message.contains(&format!("undeclared_{v}"))), "{p:?}");
    }
    c.shutdown();
}

#[test]
fn closing_a_file_outside_the_build_before_its_first_update_leaves_nothing() {
    let root = project("closeearly", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let checks = Arc::new(Checks::default());
    let hooks = {
        let g = gate.clone();
        Hooks { before_update: Some(Box::new(move || g.pass())), problem_check: Some(checks.hook()), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let scratch = uri(&root, "scratch.sv");
    // Opened while updates are held, closed before its first update is in.
    gate.close();
    c.open(&scratch, 1, "module scratch;\n  no_such_type x;\nendmodule\n");
    gate.wait_entered(1);
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": scratch } }));
    c.sync();
    gate.open();
    // The status (which waits for every earlier edit) counts no unit of its own, and its
    // problems end cleared.
    let st = c.request_ok("dvicode/status", Value::Null);
    assert_eq!(st["units"], 3, "{st}");
    checks.wait_done(&mut c);
    let last = c.published.iter().filter(|p| p.uri == scratch).last();
    assert!(last.is_none_or(|p| p.diagnostics.is_empty()), "{last:?}");
    c.shutdown();
}

#[test]
fn a_waiting_request_keeps_the_edits_it_needed_when_an_update_separates_its_file() {
    // x is declared in top.sv and used in h.svh, which top.sv includes.
    let top_text = "module top;\n  int x;\n`include \"h.svh\"\nendmodule\n";
    let root = project("separated", &[("top.sv", top_text), ("h.svh", "  initial x = 1;\n")]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let (top, hu) = (uri(&root, "top.sv"), uri(&root, "h.svh"));
    let h_text = "  initial x = 1;\n";
    c.open(&top, 1, top_text);
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    c.open(&hu, 1, h_text);
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": hu } }));
    // The include is removed (that update held), then put back, and x asked about in h.svh.
    gate.close();
    let n = gate.entered.load(Ordering::SeqCst);
    c.change(&top, 2, &top_text.replace("`include \"h.svh\"\n", ""));
    let _ = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    gate.wait_entered(n + 1);
    c.change(&top, 3, top_text);
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": hu }, "position": at(h_text, "x = 1") }));
    c.sync();
    gate.open();
    // Answered once the include is back, not from the update in between.
    let r = c.reply(&def, WAIT).expect("an answer");
    let locs = r.result.unwrap_or(Value::Null);
    assert_eq!(locs.as_array().map(|l| l.len()), Some(1), "{locs}");
    assert_eq!(locs[0]["uri"], top.as_str());
    c.shutdown();
}

#[test]
fn a_disk_change_of_a_file_the_running_update_reads_keeps_its_place() {
    let top_text = "module top;\nendmodule\n";
    let root = project("diskorder", &[("top.sv", top_text), ("h.svh", "  typedef int old_t;\n")]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let hooks = {
        let g = gate.clone();
        Hooks { after_update: Some(Box::new(move || g.pass())), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let (top, hu) = (uri(&root, "top.sv"), uri(&root, "h.svh"));
    c.open(&top, 1, top_text);
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    // The header is opened and closed, and top.sv starts to include it: one update of both
    // replaces the first, has read the old header and is held when it changes on disk.
    gate.close();
    let n = gate.entered.load(Ordering::SeqCst);
    c.open(&hu, 1, "  typedef int old_t;\n");
    gate.wait_entered(n + 1);
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": hu } }));
    c.change(&top, 2, "module top;\n`include \"h.svh\"\n  new_t v;\nendmodule\n");
    let _ = c.send_request("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    c.sync();
    gate.allow(1);
    gate.wait_entered(n + 2);
    std::fs::write(root.join("h.svh"), "  typedef int new_t;\n").unwrap();
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": hu, "type": 2 }] }));
    let found = c.send_request("workspace/symbol", json!({ "query": "new_t" }));
    c.sync();
    gate.open();
    let found = c.reply(&found, WAIT).expect("symbols of the project").result.unwrap_or(Value::Null);
    let found: Vec<&str> = found.as_array().into_iter().flatten().filter_map(|s| s["location"]["uri"].as_str()).collect();
    assert!(found.len() == 1 && found[0].ends_with("h.svh"), "{found:?}");
    c.shutdown();
}

#[test]
fn library_files_are_found_as_the_update_reads_them() {
    let root = project("libpin", &[("top.sv", TOP_LIB)]);
    std::fs::create_dir_all(root.join("early")).unwrap();
    std::fs::create_dir_all(root.join("late")).unwrap();
    std::fs::write(root.join("late/m.sv"), "module m; endmodule\n").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "top.sv\n-y early\n-y late\n").unwrap();
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, TOP_LIB);
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
    // m is instantiated (that update held); meanwhile early/m.sv is opened, not saved.
    gate.close();
    let n = gate.entered.load(Ordering::SeqCst);
    let text = TOP_LIB.replace("endmodule", "  m u();\nendmodule");
    c.change(&top, 2, &text);
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(&text, "m u()") }));
    gate.wait_entered(n + 1);
    c.open(&uri(&root, "early/m.sv"), 1, "module m; endmodule\n");
    c.sync();
    gate.open();
    // The update read the folders as they were: m is in late/.
    let r = c.reply(&def, WAIT).expect("an answer");
    let locs = r.result.unwrap_or(Value::Null);
    assert!(locs[0]["uri"].as_str().is_some_and(|u| u.ends_with("late/m.sv")), "{locs}");
    c.shutdown();
}

const TOP_LIB: &str = "module top;\nendmodule\n";

#[test]
fn a_header_opened_before_it_is_saved_is_found_by_the_files_that_include_it() {
    let top_text = "module top;\n`include \"h.svh\"\n  new_t v;\nendmodule\n";
    let root = project("unsavedheader", &[("top.sv", top_text)]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let top = uri(&root, "top.sv");
    let p = c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    assert!(codes(&p).contains(&"INCLUDE_NOT_FOUND".to_string()), "{:?}", codes(&p));
    // Created in the editor, not on disk yet.
    c.open(&uri(&root, "h.svh"), 1, "  typedef int new_t;\n");
    c.wait_problems(&top, |p| p.diagnostics.is_empty());
    c.shutdown();
}

#[test]
fn a_header_opened_while_an_update_finds_its_include_is_found_after_it() {
    let top_text = "module top;\n`include \"a.svh\"\n  new_t v;\nendmodule\n";
    let root = project("openedduring", &[("top.sv", top_text)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let checks = Arc::new(Checks::default());
    let hooks = {
        let g = gate.clone();
        Hooks { before_update: Some(Box::new(move || g.pass())), problem_check: Some(checks.hook()), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let (top, au) = (uri(&root, "top.sv"), uri(&root, "a.svh"));
    c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    // a.svh, not saved, includes b.svh; its update (of top.sv too) is held when b.svh opens.
    gate.close();
    c.open(&au, 1, "`include \"b.svh\"\n");
    gate.wait_entered(1);
    c.open(&uri(&root, "b.svh"), 1, "  typedef int new_t;\n");
    // Asked after b.svh opened: answered once top.sv is compiled with it.
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(top_text, "new_t v") }));
    c.sync();
    gate.open();
    let d = c.reply(&def, WAIT).expect("an answer").result.unwrap_or(Value::Null);
    assert!(d[0]["uri"].as_str().is_some_and(|u| u.ends_with("b.svh")), "{d}");
    // Everything settled: the include in a.svh found b.svh.
    c.request_ok("dvicode/status", Value::Null);
    checks.wait_done(&mut c);
    let last = c.published.iter().filter(|p| p.uri == au).last();
    assert!(last.is_none_or(|p| p.diagnostics.is_empty()), "{last:?}");
    c.shutdown();
}

#[test]
fn a_retry_for_a_later_header_does_not_hide_an_earlier_one() {
    let top_text = "module top;\n`include \"a.svh\"\n`include \"c.svh\"\n  new_t v;\nendmodule\n";
    let root = project("tworetries", &[("top.sv", top_text)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    gate.close();
    c.open(&uri(&root, "a.svh"), 1, "`include \"b.svh\"\n");
    gate.wait_entered(1);
    c.open(&uri(&root, "b.svh"), 1, "  typedef int new_t;\n");
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(top_text, "new_t v") }));
    c.open(&uri(&root, "c.svh"), 1, "\n");
    c.sync();
    gate.open();
    let d = c.reply(&def, WAIT).expect("an answer").result.unwrap_or(Value::Null);
    assert!(d[0]["uri"].as_str().is_some_and(|u| u.ends_with("b.svh")), "{d}");
    c.shutdown();
}

#[test]
fn a_header_opened_during_an_update_is_looked_for_while_a_build_runs() {
    let top_text = "module top;\n`include \"a.svh\"\n  new_t v;\nendmodule\n";
    let root = project("retrybuild", &[("top.sv", top_text)]);
    let (builds, updates) = (Arc::new(Gate::default()), Arc::new(Gate::default()));
    let _openers = (Opener(builds.clone()), Opener(updates.clone()));
    let hooks = {
        let (b, u) = (builds.clone(), updates.clone());
        Hooks { after_build: Some(Box::new(move || b.pass())), before_update: Some(Box::new(move || u.pass())), ..Default::default() }
    };
    let (mut c, _) = Client::start_with(&root, Value::Null, hooks);
    let top = uri(&root, "top.sv");
    c.wait_problems(&top, |p| !p.diagnostics.is_empty());
    // A full build runs (held) while a.svh is opened, its update held, and b.svh opened.
    builds.close();
    c.request_ok("dvicode/rebuild", Value::Null);
    builds.wait_entered(2);
    updates.close();
    c.open(&uri(&root, "a.svh"), 1, "`include \"b.svh\"\n");
    updates.wait_entered(1);
    c.open(&uri(&root, "b.svh"), 1, "  typedef int new_t;\n");
    let def = c.send_request("textDocument/definition", json!({ "textDocument": { "uri": top }, "position": at(top_text, "new_t v") }));
    c.sync();
    updates.open();
    // Answered with b.svh before the build is done.
    let d = c.reply(&def, WAIT).expect("an answer").result.unwrap_or(Value::Null);
    assert!(d[0]["uri"].as_str().is_some_and(|u| u.ends_with("b.svh")), "{d}");
    builds.open();
    c.shutdown();
}

#[test]
fn argument_files_of_any_name_are_watched_and_rebuild() {
    // `-f sources.list`: the client is asked to watch it, and a change to it builds again.
    let root = project("arglist", &[("a.sv", "module a_only; endmodule\n"), ("b.sv", "module b_only; endmodule\n")]);
    std::fs::write(root.join("sources.list"), "a.sv\n").unwrap();
    std::fs::write(root.join("more[1].list"), "").unwrap();
    std::fs::write(root.join(".dvicode/default.build"), "-f sources.list\n-f more[1].list\n").unwrap();
    let caps = json!({ "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true, "relativePatternSupport": true } } });
    let (mut c, _) = Client::start_full(&root, Value::Null, Hooks::default(), caps);
    c.sync();
    let registrations = |c: &Client| c.requests.iter().filter(|r| r.method == "client/registerCapability").cloned().collect::<Vec<_>>();
    let regs = registrations(&c);
    assert_eq!(regs.len(), 1);
    let watchers = regs[0].params["registrations"][0]["registerOptions"]["watchers"].as_array().cloned().unwrap_or_default();
    let patterns: Vec<&str> = watchers.iter().filter_map(|w| w["globPattern"]["pattern"].as_str()).collect();
    // Glob characters of a name match only one character each: the file, not `more1.list`.
    assert!(patterns.contains(&"sources.list") && patterns.contains(&"more?1?.list"), "{patterns:?}");
    let found = |c: &mut Client, q: &str| c.request_ok("workspace/symbol", json!({ "query": q })).as_array().map_or(0, |a| a.len());
    assert_eq!((found(&mut c, "a_only"), found(&mut c, "b_only")), (1, 0));
    // Changed, under another spelling where case does not matter.
    std::fs::write(root.join("sources.list"), "b.sv\n").unwrap();
    let name = if cfg!(windows) { "Sources.LIST" } else { "sources.list" };
    c.notify("workspace/didChangeWatchedFiles", json!({ "changes": [{ "uri": uri(&root, name), "type": 2 }] }));
    let deadline = Instant::now() + WAIT;
    while found(&mut c, "b_only") == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!((found(&mut c, "a_only"), found(&mut c, "b_only")), (0, 1));
    // The same inputs: no new registration.
    c.sync();
    assert_eq!(registrations(&c).len(), 1);
    c.shutdown();
}

#[test]
fn watcher_patterns_and_input_identity() {
    use std::path::Path;
    if cfg!(windows) {
        // No verbatim prefix, forward slashes, glob characters of folder names as `?`.
        assert_eq!(super::absolute_pattern(Path::new(r"\\?\C:\p[1]\sources.list")).as_deref(), Some("C:/p?1?"));
        // A deleted input under another spelling is still the input.
        assert!(super::same_input(Path::new(r"C:\nowhere\SOURCES.list"), Path::new(r"c:\nowhere\sources.list")));
    } else {
        // A backslash is part of a name here: one character of the pattern.
        assert_eq!(super::absolute_pattern(Path::new(r"/p/a\b/sources.list")).as_deref(), Some("/p/a?b"));
        assert!(!super::same_input(Path::new("/nowhere/SOURCES.list"), Path::new("/nowhere/sources.list")) || cfg!(target_os = "macos"));
    }
}

#[test]
fn a_saved_then_closed_file_keeps_the_saved_text() {
    // The build read the old text from disk; the editor saves a new one and closes the file
    // (no watched-file notification): the project has the saved text.
    let root = project("saveclose", &[("p.sv", "module old_name; endmodule\n")]);
    let (mut c, _) = Client::start(&root, Value::Null);
    let u = uri(&root, "p.sv");
    let new = "module new_name; endmodule\n";
    c.open(&u, 1, "module old_name; endmodule\n");
    c.change(&u, 2, new);
    std::fs::write(root.join("p.sv"), new).unwrap();
    c.notify("textDocument/didSave", json!({ "textDocument": { "uri": u } }));
    c.notify("textDocument/didClose", json!({ "textDocument": { "uri": u } }));
    let found = |c: &mut Client, q: &str| c.request_ok("workspace/symbol", json!({ "query": q })).as_array().map_or(0, |a| a.len());
    let deadline = Instant::now() + WAIT;
    while found(&mut c, "new_name") == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!((found(&mut c, "new_name"), found(&mut c, "old_name")), (1, 0));
    c.shutdown();
}

const BODY: &str = "module top;\n  int alpha_count;\n  function void g(int first, int second);\n  endfunction\n  function void f();\n    int beta_local;\n    \n  endfunction\nendmodule\n";

/// `BODY` with line 6 (the empty one in `f`) replaced by `line`.
fn body_with(line: &str) -> String {
    BODY.replacen("    \n  endfunction", &format!("{line}\n  endfunction"), 1)
}

#[test]
fn typing_on_one_line_of_a_body_is_answered_without_waiting_for_the_update() {
    let root = project("linelocal", &[("top.sv", BODY)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, BODY);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    // Updates stop at the gate from now on.
    gate.close();
    c.change(&top, 2, &body_with("    alpha_count = be"));
    let r = c.send_request("textDocument/completion", json!({ "textDocument": { "uri": top }, "position": { "line": 6, "character": 20 } }));
    let r = c.reply(&r, Duration::from_secs(5)).expect("answered while the update waits");
    let items = r.result.unwrap_or(Value::Null);
    let labels: Vec<&str> = items["items"].as_array().map(|v| v.iter().filter_map(|i| i["label"].as_str()).collect()).unwrap_or_default();
    // The local of `f`: the scope is the one of the cursor.
    assert!(labels.contains(&"beta_local"), "{labels:?}");
    // Signature help on the same line, two edits later.
    c.change(&top, 3, &body_with("    g(1"));
    c.change(&top, 4, &body_with("    g(1, "));
    let s = c.send_request("textDocument/signatureHelp", json!({ "textDocument": { "uri": top }, "position": { "line": 6, "character": 9 } }));
    let s = c.reply(&s, Duration::from_secs(5)).expect("answered while the update waits");
    let s = s.result.unwrap_or(Value::Null);
    assert_eq!(s["activeParameter"], 1, "{s}");
    assert!(s["signatures"][0]["label"].as_str().unwrap_or("").contains("second"), "{s}");
    // A new line is not an edit of one line: that one waits for its update.
    c.change(&top, 5, &body_with("    g(1, 2);\n    alp"));
    let w = c.send_request("textDocument/completion", json!({ "textDocument": { "uri": top }, "position": { "line": 7, "character": 7 } }));
    assert!(c.reply(&w, Duration::from_millis(300)).is_none(), "answered before its edit was analysed");
    gate.open();
    assert!(c.reply(&w, WAIT).is_some_and(|r| r.error.is_none()));
    c.shutdown();
}

#[test]
fn a_line_that_opens_a_scope_waits_for_its_update() {
    let root = project("linelocalscope", &[("top.sv", BODY)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, BODY);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    gate.close();
    c.change(&top, 2, &body_with("    begin int gamma; gam"));
    let w = c.send_request("textDocument/completion", json!({ "textDocument": { "uri": top }, "position": { "line": 6, "character": 24 } }));
    assert!(c.reply(&w, Duration::from_millis(300)).is_none(), "answered before its edit was analysed");
    gate.open();
    assert!(c.reply(&w, WAIT).is_some_and(|r| r.error.is_none()));
    c.shutdown();
}

#[test]
fn a_line_that_ends_a_loop_waits_for_its_update() {
    // The loop's statement ends the loop on its line: past it, the loop's variable is gone.
    let text = "module top;\n  function void f();\n    for (int loop_index = 0; loop_index < 2; loop_index++)\n      f();\n    f();\n  endfunction\nendmodule\n";
    let root = project("linelocalloop", &[("top.sv", text)]);
    let gate = Arc::new(Gate::default());
    let _opener = Opener(gate.clone());
    let (mut c, _) = Client::start_with(&root, Value::Null, gate.hooks());
    let top = uri(&root, "top.sv");
    c.open(&top, 1, text);
    gate.wait_entered(1);
    c.wait_note("dvicode/revision", |_| true);
    gate.close();
    c.change(&top, 2, &text.replace("      f();\n    f();", "      f(); loo\n    f();"));
    let w = c.send_request("textDocument/completion", json!({ "textDocument": { "uri": top }, "position": { "line": 3, "character": 14 } }));
    assert!(c.reply(&w, Duration::from_millis(300)).is_none(), "answered before its edit was analysed");
    gate.open();
    assert!(c.reply(&w, WAIT).is_some_and(|r| r.error.is_none()));
    c.shutdown();
}
