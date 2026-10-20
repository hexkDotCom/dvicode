//! The server through its message loop, as an editor drives it: an in-memory connection, a
//! small project on disk, and the notifications and replies in the order they arrive.

use super::run;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{PublishDiagnosticsParams, Url};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Once;
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
}

impl Client {
    /// Starts a server on `root` and waits for its first build.
    fn start(root: &Path, options: Value) -> (Client, Value) {
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
        let handle = std::thread::spawn(move || run(server).expect("server loop"));
        let mut c = Client { conn, server: Some(handle), next_id: 0, notes: Vec::new(), published: Vec::new() };
        let root_uri = Url::from_file_path(root).unwrap();
        let init = c.request_ok(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": "test" }],
                "capabilities": {},
                "initializationOptions": options,
            }),
        );
        c.notify("initialized", json!({}));
        c.wait_note("dvicode/status", |_| true);
        (c, init)
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
            // Requests of the server (none expected without client capabilities) are acknowledged.
            Message::Request(r) => {
                self.conn.sender.send(Message::Response(Response::new_ok(r.id.clone(), Value::Null))).unwrap();
            }
            _ => {}
        }
        Some(m)
    }

    fn request(&mut self, method: &str, params: Value) -> Response {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.conn.sender.send(Message::Request(Request::new(id.clone(), method.into(), params))).unwrap();
        let deadline = Instant::now() + WAIT;
        loop {
            match self.recv(deadline) {
                Some(Message::Response(r)) if r.id == id => return r,
                Some(Message::Notification(n)) => self.notes.push(n),
                Some(_) => {}
                None => panic!("no reply to {method}"),
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
fn initialize_announces_capabilities_and_builds() {
    let root = project("init", &[("pkg.sv", PKG), ("top.sv", TOP)]);
    let (c, init) = Client::start(&root, Value::Null);
    let caps = &init["capabilities"];
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["textDocumentSync"]["change"], 2, "incremental sync");
    assert_eq!(caps["renameProvider"]["prepareProvider"], true);
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
    let (mut c, _) = Client::start(&root, Value::Null);
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
    // Requests are answered from the newest text, so everything published so far is in.
    c.request_ok("textDocument/documentSymbol", json!({ "textDocument": { "uri": top } }));
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
