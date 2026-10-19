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
