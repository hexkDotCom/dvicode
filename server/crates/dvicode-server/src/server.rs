//! The LSP main loop.

mod build_docs;

use crate::{crash, fs, util};
use build_docs::{BuildDoc, Inspected};
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use dvi_analysis::ide::tokens::{TOKEN_MODIFIERS, TOKEN_TYPES};
use dvi_analysis::ide::{path_of, url_of};
use dvi_analysis::{Analysis, ProjectSetup, Vfs};
use dvi_project::Progress;
use dvi_syntax::{FileId, SourceProvider};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Res<T> = Result<T, Box<dyn Error + Sync + Send>>;

const DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Build configuration file, relative to the workspace root or absolute.
    pub build_file: Option<String>,
    /// Extra environment variables for build file expansion.
    pub env: FxHashMap<String, String>,
    /// UVM library location used by `-uvm` and auto-configuration.
    pub uvm_home: Option<String>,
    /// Maximum number of problems published after a full build (0 = no limit).
    pub max_problems: Option<usize>,
}

/// For the log, which also goes into issue reports: variable names only, since `dvicode.env`
/// may hold license keys or passwords.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut env: Vec<&str> = self.env.keys().map(|k| k.as_str()).collect();
        env.sort_unstable();
        f.debug_struct("Settings")
            .field("build_file", &self.build_file)
            .field("env", &env)
            .field("uvm_home", &self.uvm_home)
            .field("max_problems", &self.max_problems)
            .finish()
    }
}

fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF16),
        text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions { include_text: Some(false) })),
            ..Default::default()
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into(), "`".into()]),
            ..Default::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".into(), ",".into()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        definition_provider: Some(OneOf::Left(true)),
        declaration_provider: Some(DeclarationCapability::Simple(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions { prepare_provider: Some(true), work_done_progress_options: Default::default() })),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        })),
        document_link_provider: Some(DocumentLinkOptions { resolve_provider: Some(false), work_done_progress_options: Default::default() }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
            legend: SemanticTokensLegend { token_types: TOKEN_TYPES.to_vec(), token_modifiers: TOKEN_MODIFIERS.to_vec() },
            full: Some(SemanticTokensFullOptions::Bool(true)),
            range: None,
            work_done_progress_options: Default::default(),
        })),
        // What the views can ask for, so a newer extension with an older engine shows only what
        // it supports.
        experimental: Some(json!({ "dvicode": { "verificationObjects": 1, "structure": 1 } })),
        ..Default::default()
    }
}

/// What has been published, and which publishing job may publish each file.
#[derive(Default)]
struct PubState {
    /// Files that currently show problems in the client.
    published: FxHashSet<Url>,
    /// File -> sequence number of the newest job that covers it.
    latest: FxHashMap<Url, u64>,
    next: u64,
}

enum Event {
    /// A finished full build (`None`: cancelled for a newer one); the error message when it
    /// failed and fell back to an empty project.
    Built(Option<Box<Analysis>>, u64, Option<String>),
    /// An open build or argument file checked: its URI and version, and what was found.
    Inspected(Url, u64, Inspected),
}

struct Server {
    sender: Sender<Message>,
    vfs: Arc<Vfs>,
    analysis: Arc<Analysis>,
    root: Option<PathBuf>,
    settings: Settings,
    open: FxHashMap<FileId, Url>,
    pending: FxHashSet<FileId>,
    pending_since: Option<Instant>,
    events_tx: Sender<Event>,
    events_rx: Receiver<Event>,
    building: bool,
    /// Files changed since the running full build started; replayed onto its result.
    changed_during_build: FxHashSet<FileId>,
    build_again: bool,
    build_generation: u64,
    /// Revision of the current analysis (see `Server::install`).
    revision: u64,
    rebuild_at: Option<Instant>,
    built_once: bool,
    published: Arc<Mutex<PubState>>,
    next_id: i32,
    /// The client shows `$/progress` notifications.
    progress: bool,
    /// Progress of the running build, and the switch that cancels it.
    build_progress: Option<Arc<Progress>>,
    build_started: Instant,
    /// What the client was last told (message, percentage), and when progress was last logged.
    reported: (String, u32),
    logged_at: Instant,
    /// Build and argument files open in the editor, and when to check them again.
    build_docs: FxHashMap<Url, BuildDoc>,
    inspect_at: Option<Instant>,
    /// Numbers the checks of build documents (`BuildDoc::seq`).
    inspect_seq: u64,
}

/// How long typing in a build file pauses before it is checked.
const INSPECT_DELAY: Duration = Duration::from_millis(300);

/// How often the running build's progress is sent to the client, and written to the log.
const REPORT_EVERY: Duration = Duration::from_millis(500);
const LOG_EVERY: Duration = Duration::from_secs(5);

pub fn run(conn: Connection) -> Res<()> {
    let (id, params) = conn.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo { name: "dvicode-server".into(), version: Some(crate::VERSION.into()) }),
    };
    conn.initialize_finish(id, serde_json::to_value(result)?)?;

    #[allow(deprecated)]
    let root = params
        .workspace_folders
        .as_ref()
        .and_then(|w| w.first())
        .and_then(|f| path_of(&f.uri))
        .or_else(|| params.root_uri.as_ref().and_then(path_of))
        .or_else(|| params.root_path.as_ref().map(PathBuf::from));
    let settings: Settings = params
        .initialization_options
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let progress = params.capabilities.window.as_ref().and_then(|w| w.work_done_progress).unwrap_or(false);
    log::info!("workspace root: {:?}; settings: {:?}", root, settings);
    let vfs = Arc::new(Vfs::new());
    let empty_setup = Arc::new(ProjectSetup::new(root.as_deref().unwrap_or(std::path::Path::new("/")), Default::default(), Default::default()));
    let analysis = Arc::new(Analysis::build(vfs.clone(), empty_setup));
    let (events_tx, events_rx) = unbounded();
    let mut s = Server {
        sender: conn.sender.clone(),
        vfs,
        analysis,
        root,
        settings,
        open: FxHashMap::default(),
        pending: FxHashSet::default(),
        pending_since: None,
        events_tx,
        events_rx,
        building: false,
        changed_during_build: FxHashSet::default(),
        build_again: false,
        build_generation: 0,
        revision: 0,
        rebuild_at: None,
        built_once: false,
        published: Arc::new(Mutex::new(PubState::default())),
        next_id: 1,
        progress,
        build_progress: None,
        build_started: Instant::now(),
        reported: (String::new(), 0),
        logged_at: Instant::now(),
        build_docs: FxHashMap::default(),
        inspect_at: None,
        inspect_seq: 0,
    };
    s.start_build();
    s.main_loop(&conn)
}

impl Server {
    fn main_loop(&mut self, conn: &Connection) -> Res<()> {
        loop {
            let mut timeout = Duration::from_secs(3600);
            if let Some(t) = self.pending_since {
                timeout = timeout.min(DEBOUNCE.saturating_sub(t.elapsed()));
            }
            if let Some(t) = self.rebuild_at {
                timeout = timeout.min(t.saturating_duration_since(Instant::now()));
            }
            if self.building {
                timeout = timeout.min(REPORT_EVERY);
            }
            if let Some(t) = self.inspect_at {
                timeout = timeout.min(t.saturating_duration_since(Instant::now()));
            }
            select! {
                recv(conn.receiver) -> msg => {
                    let Ok(msg) = msg else { return Ok(()) };
                    match msg {
                        Message::Request(req) => {
                            if conn.handle_shutdown(&req)? {
                                return Ok(());
                            }
                            let (id, method) = (req.id.clone(), req.method.clone());
                            let tx = self.sender.clone();
                            if crash::catch(&method, Some(&tx), || self.on_request(req)).is_none() {
                                self.send(Message::Response(Response::new_err(id, ErrorCode::InternalError as i32, format!("{method} failed"))));
                            }
                        }
                        Message::Notification(n) => {
                            if n.method == "exit" {
                                return Ok(());
                            }
                            let method = n.method.clone();
                            let tx = self.sender.clone();
                            crash::catch(&method, Some(&tx), || self.on_notification(n));
                        }
                        Message::Response(_) => {}
                    }
                }
                recv(self.events_rx) -> ev => {
                    match ev {
                        Ok(Event::Built(a, generation, error)) => self.on_built(a.map(|a| *a), generation, error),
                        Ok(Event::Inspected(uri, seq, ins)) => self.on_inspected(uri, seq, ins),
                        Err(_) => {}
                    }
                }
                default(timeout) => {}
            }
            if self.pending_since.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
                self.flush_pending();
            }
            if self.rebuild_at.is_some_and(|t| Instant::now() >= t) {
                self.rebuild_at = None;
                self.start_build();
            }
            if self.building {
                self.report_build_progress();
            }
            if self.inspect_at.is_some_and(|t| Instant::now() >= t) {
                self.inspect_at = None;
                self.inspect_build_docs();
            }
        }
    }

    /// Checks the open build and argument files in the background, the way the current build
    /// reads them.
    fn inspect_build_docs(&mut self) {
        let env = project_env(&self.settings);
        let uris: Vec<Url> = self.build_docs.keys().cloned().collect();
        for uri in uris {
            // Each check has a number: only the newest one of a document is kept (another may
            // still run with an older build's context, or for the text before a reopen).
            self.inspect_seq += 1;
            let seq = self.inspect_seq;
            let job = self.inspect_job(&uri, &env, seq);
            let Some(job) = job else { continue };
            let tx = self.events_tx.clone();
            rayon::spawn(move || {
                if let Some(ins) = job() {
                    let _ = tx.send(Event::Inspected(uri, seq, ins));
                }
            });
        }
    }

    /// A check of the open build document `uri` as it is now, to run in the background; marks
    /// it as the document's newest check `seq`.
    fn inspect_job(&mut self, uri: &Url, env: &FxHashMap<String, String>, seq: u64) -> Option<impl FnOnce() -> Option<Inspected> + Send + 'static> {
        let cx = {
            let doc = self.build_docs.get(uri)?;
            build_docs::context(&doc.path, self.root.as_deref(), &self.analysis.setup.config, env)
        };
        let doc = self.build_docs.get_mut(uri)?;
        if seq != 0 {
            doc.seq = seq;
        }
        let (version, path, text, env, sender) = (doc.version, doc.path.clone(), doc.text.clone(), env.clone(), self.sender.clone());
        Some(move || {
            let inspection = crash::catch("build file check", Some(&sender), || build_docs::inspect(&path, &text, &cx, &env))?;
            let lines = dvi_syntax::LineIndex::new(&text);
            Some(Inspected { text, lines, version, inspection })
        })
    }

    fn on_inspected(&mut self, uri: Url, seq: u64, ins: Inspected) {
        let Some(doc) = self.build_docs.get_mut(&uri) else { return };
        if doc.seq != seq || doc.version != ins.version {
            return; // a newer check is coming
        }
        let version = ins.version;
        let diagnostics = build_docs::diagnostics(&ins);
        doc.inspected = Some(ins);
        let mut st = self.published.lock().unwrap();
        st.next += 1;
        let g = st.next;
        st.latest.insert(uri.clone(), g);
        if diagnostics.is_empty() {
            st.published.remove(&uri);
        } else {
            st.published.insert(uri.clone());
        }
        let p = PublishDiagnosticsParams { uri, diagnostics, version: Some(version) };
        let _ = self.sender.send(Message::Notification(Notification::new("textDocument/publishDiagnostics".into(), p)));
    }

    /// Sends the running build's progress to the client when it changed, and logs it now and
    /// then, so that a long build shows what it is doing.
    fn report_build_progress(&mut self) {
        let Some(p) = self.build_progress.clone() else { return };
        let (message, percentage) = (p.message(), p.percentage());
        if self.progress && (self.reported.0 != message || self.reported.1 != percentage) {
            self.notify(
                "$/progress",
                ProgressParams {
                    token: NumberOrString::String("dvicode-build".into()),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(WorkDoneProgressReport {
                        cancellable: Some(false),
                        message: Some(message.clone()),
                        percentage: Some(percentage),
                    })),
                },
            );
            self.reported = (message.clone(), percentage);
        }
        if self.logged_at.elapsed() >= LOG_EVERY {
            self.logged_at = Instant::now();
            log::info!("building ({} s): {message}", self.build_started.elapsed().as_secs());
        }
    }

    fn send(&self, m: Message) {
        let _ = self.sender.send(m);
    }

    fn notify(&self, method: &str, params: impl Serialize) {
        self.send(Message::Notification(Notification::new(method.into(), params)));
    }

    fn request_client(&mut self, method: &str, params: impl Serialize) {
        let id = RequestId::from(format!("dvicode-{}", self.next_id));
        self.next_id += 1;
        self.send(Message::Request(Request::new(id, method.into(), params)));
    }

    // ------------------------------------------------------------------ builds

    fn start_build(&mut self) {
        if self.building {
            // The running build is out of date (new build file, settings, Rebuild Project):
            // stop it, and start again when it has stopped.
            if !self.build_again {
                log::info!("cancelling the running build for a new one");
            }
            if let Some(p) = &self.build_progress {
                p.cancel();
            }
            self.build_again = true;
            return;
        }
        self.building = true;
        self.changed_during_build.clear();
        self.build_generation += 1;
        let generation = self.build_generation;
        let progress = Arc::new(Progress::new());
        self.build_progress = Some(progress.clone());
        self.build_started = Instant::now();
        self.logged_at = Instant::now();
        self.reported = (String::new(), 0);
        if self.progress {
            self.request_client("window/workDoneProgress/create", WorkDoneProgressCreateParams { token: NumberOrString::String("dvicode-build".into()) });
            self.notify(
                "$/progress",
                ProgressParams {
                    token: NumberOrString::String("dvicode-build".into()),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                        title: "DVICode: building project".into(),
                        cancellable: Some(false),
                        message: None,
                        percentage: None,
                    })),
                },
            );
        }
        let (root, settings) = (self.root.clone(), self.settings.clone());
        let vfs = self.vfs.clone();
        let tx = self.events_tx.clone();
        let client = self.sender.clone();
        std::thread::Builder::new()
            .name("dvicode-build".into())
            .stack_size(64 << 20)
            .spawn(move || {
                // Configuring can take long too (auto-configuration reads the whole folder), so
                // it runs here, not in the loop that answers the editor.
                let t0 = Instant::now();
                let setup = Arc::new(project_setup(root.as_deref(), &settings, &progress));
                let t1 = Instant::now();
                if !progress.is_cancelled() {
                    log::info!("configuration: {} in {} ms", describe_config(&setup), (t1 - t0).as_millis());
                }
                let r = crash::catch("project build", Some(&client), || Analysis::build_with(vfs.clone(), setup.clone(), &progress));
                let (a, error) = match r {
                    Some(Some(a)) => (Some(a), None),
                    Some(None) => (None, None),
                    None => {
                        log::error!("falling back to an empty project");
                        let empty = Analysis::build(vfs, Arc::new(ProjectSetup::new(&setup.root, Default::default(), Default::default())));
                        (Some(empty), Some("The project build failed with an internal error".to_string()))
                    }
                };
                match &a {
                    Some(a) => log::info!(
                        "full build: {} units; configuration {} ms, build {} ms",
                        a.units.len(),
                        (t1 - t0).as_millis(),
                        t1.elapsed().as_millis()
                    ),
                    None => log::info!("build cancelled after {} ms", t0.elapsed().as_millis()),
                }
                let _ = tx.send(Event::Built(a.map(Box::new), generation, error));
            })
            .expect("spawn build thread");
    }

    fn on_built(&mut self, a: Option<Analysis>, _generation: u64, error: Option<String>) {
        self.building = false;
        self.build_progress = None;
        let Some(a) = a else {
            // Cancelled for a newer build, which starts now with a progress of its own.
            self.end_progress();
            if self.build_again {
                self.build_again = false;
                self.start_build();
            }
            return;
        };
        // Tell when the build file in use changed (one added, removed or chosen).
        let new_bf = a.setup.config.build_file.clone();
        if self.built_once && self.analysis.setup.config.build_file != new_bf {
            let text = match &new_bf {
                Some(bf) => {
                    let shown = self.root.as_deref().and_then(|r| bf.strip_prefix(r).ok()).unwrap_or(bf);
                    format!("DVICode now uses the build file {}", shown.display())
                }
                None => "DVICode now configures the project automatically (no build file)".to_string(),
            };
            log::info!("{text}");
            self.notify("window/showMessage", ShowMessageParams { typ: MessageType::INFO, message: text });
        }
        // Open files that are not part of the build are compiled ad hoc.
        let open = self.open_files();
        let mut a = a.sync_open_files(&open);
        // Edits made while the build ran, including those already applied to the previous
        // snapshot by the debounce: the build may have read older text.
        let mut files: Vec<FileId> = self.changed_during_build.drain().collect();
        files.extend(self.pending.drain());
        files.sort_unstable();
        files.dedup();
        self.pending_since = None;
        if !files.is_empty() {
            let built = &a;
            let replayed = crash::catch("update after an edit", Some(&self.sender), || built.update(&files).sync_open_files(&open));
            if let Some(next) = replayed {
                a = next;
            }
        }
        self.install(a);
        self.built_once = true;
        self.end_progress();
        let st = self.analysis.status();
        log::info!(
            "project ready: {} top files, {} files, {} declarations; {} s since the build started",
            st.top_files,
            st.files,
            st.declarations,
            self.build_started.elapsed().as_secs_f32().round()
        );
        // The client shows a failed build in its status bar.
        let mut status = serde_json::to_value(&st).unwrap_or_default();
        if let (Some(e), Some(o)) = (error, status.as_object_mut()) {
            o.insert("build_error".into(), Value::String(e));
        }
        self.notify("dvicode/status", &status);
        self.publish_all();
        // The build may read open argument files differently now.
        if !self.build_docs.is_empty() {
            self.inspect_at = Some(Instant::now());
        }
        if self.build_again {
            self.build_again = false;
            self.start_build();
        }
    }

    fn end_progress(&self) {
        if self.progress {
            self.notify(
                "$/progress",
                ProgressParams {
                    token: NumberOrString::String("dvicode-build".into()),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd { message: None })),
                },
            );
        }
    }

    /// The open documents, in a stable order.
    fn open_files(&self) -> Vec<FileId> {
        let mut v: Vec<FileId> = self.open.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Records that a file's text changed: analysed after the debounce, and replayed onto
    /// the result of a running full build.
    fn note_change(&mut self, id: FileId) {
        self.pending.insert(id);
        self.pending_since.get_or_insert_with(Instant::now);
        if self.building {
            self.changed_during_build.insert(id);
        }
    }

    fn flush_pending(&mut self) {
        self.pending_since = None;
        if self.pending.is_empty() {
            return;
        }
        let files: Vec<FileId> = self.pending.drain().collect();
        let t0 = Instant::now();
        let a = self.analysis.clone();
        let open = self.open_files();
        // An open header may have lost its last includer, or an open file outside the build
        // gained one.
        let r = crash::catch("update after an edit", Some(&self.sender), || a.update(&files).sync_open_files(&open));
        let mut interface_changed = false;
        let mut recompiled = Vec::new();
        match r {
            Some(next) => {
                interface_changed = next.interface_changed(&a);
                recompiled = next.recompiled_files(&a);
                self.install(next);
                // The views refresh what they show.
                self.notify("dvicode/revision", &json!({ "revision": self.revision }));
            }
            None => log::error!("kept the previous analysis of {:?}", files.iter().map(|f| self.vfs.path(*f)).collect::<Vec<_>>()),
        }
        if interface_changed {
            // Declarations or macros other files use changed: their problems may have too.
            self.publish_all();
            return;
        }
        log::debug!("incremental update of {} file(s) in {} ms", files.len(), t0.elapsed().as_millis());
        // Problems of the changed files, of the other files compiled again with them, and of
        // the open files.
        let mut targets: FxHashSet<FileId> = files.iter().copied().collect();
        targets.extend(recompiled);
        for f in self.open.keys() {
            targets.insert(*f);
        }
        // Their SKIPPED_CODE notices count uses in every file (no build file).
        targets.extend(self.analysis.files_with_skipped_code());
        self.publish(targets.into_iter().collect(), false);
    }

    fn flush_if_pending(&mut self, params: &Value) {
        if self.pending.is_empty() {
            return;
        }
        let uri = params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| u.as_str());
        let touches = match uri.and_then(|u| Url::parse(u).ok()).and_then(|u| self.analysis.file_of_url(&u)) {
            Some(f) => self.pending.contains(&f),
            None => true,
        };
        if touches {
            self.flush_pending();
        }
    }

    // ------------------------------------------------------------------ diagnostics

    fn publish_all(&mut self) {
        let mut files = self.analysis.project_files();
        for f in self.open.keys() {
            if !files.contains(f) {
                files.push(*f);
            }
        }
        // Build files with problems (the client does not open them as documents).
        for p in &self.analysis.setup.config.problems {
            let f = self.vfs.file_id(&p.file);
            if !files.contains(&f) {
                files.push(f);
            }
        }
        self.publish(files, true);
    }

    /// Computes and publishes problems in the background. With `full`, files that had
    /// problems before and are not in `files` are cleared.
    ///
    /// Jobs run concurrently and may finish in any order, so each job gets a sequence number
    /// and only publishes a file while it is the newest job covering that file.
    fn publish(&self, files: Vec<FileId>, full: bool) {
        let a = self.analysis.clone();
        // Open build files have problems of their own, as typed (`on_inspected`).
        let skip: FxHashSet<Url> = self.build_docs.keys().cloned().collect();
        let files: Vec<FileId> = files.into_iter().filter(|f| skip.is_empty() || !skip.contains(&url_of(&a.vfs.path(*f)))).collect();
        let sender = self.sender.clone();
        let published = self.published.clone();
        let generation = {
            let mut st = published.lock().unwrap();
            st.next += 1;
            let g = st.next;
            for f in &files {
                st.latest.insert(url_of(&a.vfs.path(*f)), g);
            }
            g
        };
        let open: FxHashSet<FileId> = self.open.keys().copied().collect();
        let limit = self.settings.max_problems.unwrap_or(1000);
        rayon::spawn(move || {
            let mut total = 0usize;
            let mut now: FxHashSet<Url> = FxHashSet::default();
            let mut files = files;
            // Open files first so they are never cut by the limit.
            files.sort_by_key(|f| !open.contains(f));
            for f in files {
                if a.vfs.is_builtin(f) {
                    continue;
                }
                let Some(diags) = crash::catch("problem check", Some(&sender), || a.file_diagnostics(f)) else {
                    log::error!("no problems shown for {}", a.vfs.path(f).display());
                    continue;
                };
                let mut lsp: Vec<Diagnostic> = diags.iter().filter_map(|d| util::to_lsp_diag(&a, d)).collect();
                if !open.contains(&f) && limit > 0 {
                    if total >= limit {
                        lsp.clear();
                    } else {
                        lsp.truncate(limit - total);
                    }
                }
                total += lsp.len();
                let uri = url_of(&a.vfs.path(f));
                // Check and send under the lock, so a newer job cannot publish in between.
                let mut st = published.lock().unwrap();
                if st.latest.get(&uri) != Some(&generation) {
                    continue; // a newer job owns this file
                }
                now.insert(uri.clone());
                if lsp.is_empty() && !st.published.contains(&uri) {
                    continue;
                }
                if lsp.is_empty() {
                    st.published.remove(&uri);
                } else {
                    st.published.insert(uri.clone());
                }
                // The document version these problems belong to (open files only).
                let version = a.source(f).and_then(|s| s.lsp_version);
                let p = PublishDiagnosticsParams { uri, diagnostics: lsp, version };
                let _ = sender.send(Message::Notification(Notification::new("textDocument/publishDiagnostics".into(), p)));
            }
            if full {
                let mut st = published.lock().unwrap();
                let stale: Vec<Url> = st
                    .published
                    .iter()
                    .filter(|u| !now.contains(*u) && !skip.contains(*u) && st.latest.get(*u).map_or(true, |g| *g <= generation))
                    .cloned()
                    .collect();
                for u in stale {
                    st.published.remove(&u);
                    st.latest.insert(u.clone(), generation);
                    let p = PublishDiagnosticsParams { uri: u, diagnostics: Vec::new(), version: None };
                    let _ = sender.send(Message::Notification(Notification::new("textDocument/publishDiagnostics".into(), p)));
                }
            }
        });
    }

    // ------------------------------------------------------------------ notifications

    fn on_notification(&mut self, n: Notification) {
        match n.method.as_str() {
            "textDocument/didOpen" => {
                let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params) else { return };
                let Some(path) = path_of(&p.text_document.uri) else { return };
                if build_docs::is_build_doc(&path, Some(&p.text_document.language_id)) {
                    let doc = BuildDoc { path: path.clone(), text: Arc::from(p.text_document.text), version: p.text_document.version, seq: 0, inspected: None };
                    self.build_docs.insert(url_of(&path), doc);
                    self.inspect_at = Some(Instant::now());
                    return;
                }
                let id =self.vfs.set_overlay_versioned(&path, Arc::from(p.text_document.text), Some(p.text_document.version));
                self.open.insert(id, p.text_document.uri);
                self.note_change(id);
                // Make the file available right away.
                self.flush_pending();
            }
            "textDocument/didChange" => {
                let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params) else { return };
                let Some(path) = path_of(&p.text_document.uri) else { return };
                if let Some(doc) = self.build_docs.get_mut(&url_of(&path)) {
                    doc.text = Arc::from(util::apply_changes(&doc.text, p.content_changes));
                    doc.version = p.text_document.version;
                    self.inspect_at = Some(Instant::now() + INSPECT_DELAY);
                    return;
                }
                let id =self.vfs.file_id(&path);
                let old = self.vfs.text(id).unwrap_or_else(|| Arc::from(""));
                let new = util::apply_changes(&old, p.content_changes);
                self.vfs.set_overlay_versioned(&path, Arc::from(new), Some(p.text_document.version));
                self.note_change(id);
                self.pending_since = Some(Instant::now());
            }
            "textDocument/didSave" => {
                let Ok(p) = serde_json::from_value::<DidSaveTextDocumentParams>(n.params) else { return };
                let Some(path) = path_of(&p.text_document.uri) else { return };
                // Macro changes reach later files through incremental updates; only a changed
                // build configuration needs a full build.
                let is_build = path.extension().is_some_and(|e| e == "build" || e == "f" || e == "F");
                if is_build {
                    self.rebuild_at = Some(Instant::now() + Duration::from_millis(800));
                }
            }
            "textDocument/didClose" => {
                let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params) else { return };
                let Some(path) = path_of(&p.text_document.uri) else { return };
                if self.build_docs.remove(&url_of(&path)).is_some() {
                    // Back to the problems the last build found in it, if any.
                    let uri = url_of(&path);
                    {
                        let mut st = self.published.lock().unwrap();
                        st.next += 1;
                        let g = st.next;
                        st.latest.insert(uri.clone(), g);
                        st.published.remove(&uri);
                    }
                    let msg = PublishDiagnosticsParams { uri, diagnostics: Vec::new(), version: None };
                    self.notify("textDocument/publishDiagnostics", msg);
                    if self.analysis.setup.config.problems.iter().any(|p| p.file == path) {
                        self.publish(vec![self.vfs.file_id(&path)], false);
                    }
                    return;
                }
                self.vfs.clear_overlay(&path);
                if let Some(id) = self.vfs.lookup(&path) {
                    self.open.remove(&id);
                    if self.analysis.is_adhoc_root(id) {
                        let next = self.analysis.remove_adhoc(id);
                        self.install(next);
                        let uri = p.text_document.uri;
                        let mut st = self.published.lock().unwrap();
                        // Supersede any job still computing problems for this file.
                        st.next += 1;
                        let g = st.next;
                        st.latest.insert(uri.clone(), g);
                        st.published.remove(&uri);
                        let msg = PublishDiagnosticsParams { uri, diagnostics: Vec::new(), version: None };
                        let _ = self.sender.send(Message::Notification(Notification::new("textDocument/publishDiagnostics".into(), msg)));
                        drop(st);
                        // Other files may have used its declarations or macros.
                        self.publish_all();
                    }
                    // Back to the disk contents, also in the units of other open files that
                    // include it.
                    self.note_change(id);
                }
            }
            "workspace/didChangeWatchedFiles" => {
                let Ok(p) = serde_json::from_value::<DidChangeWatchedFilesParams>(n.params) else { return };
                let auto = self.analysis.setup.config.build_file.is_none();
                // Files created or deleted change what open build files find.
                if !self.build_docs.is_empty() && p.changes.iter().any(|c| c.typ != FileChangeType::CHANGED) {
                    self.inspect_at = Some(Instant::now() + INSPECT_DELAY);
                }
                for c in p.changes {
                    let Some(path) = path_of(&c.uri) else { continue };
                    let is_build = path.extension().is_some_and(|e| e == "build" || e == "f" || e == "F");
                    if is_build {
                        self.rebuild_at = Some(Instant::now() + Duration::from_millis(500));
                        continue;
                    }
                    let id = self.vfs.invalidate(&path);
                    // A new or deleted source changes what auto-configuration finds, which files
                    // a build-file wildcard matches, and whether an `include resolves.
                    if c.typ != FileChangeType::CHANGED && (auto || is_sv_file(&path)) {
                        self.rebuild_at = Some(Instant::now() + Duration::from_millis(1500));
                    }
                    if let Some(id) = id {
                        if !self.vfs.has_overlay(id) {
                            if self.building {
                                // The running build may have read the old text, even of a file
                                // the previous snapshot did not know: replay it afterwards.
                                self.changed_during_build.insert(id);
                            }
                            if !self.analysis.units_of_file(id).is_empty() {
                                self.note_change(id);
                            }
                        }
                    }
                }
            }
            "workspace/didChangeConfiguration" => {
                let Ok(p) = serde_json::from_value::<DidChangeConfigurationParams>(n.params) else { return };
                let v = p.settings.get("dvicode").cloned().unwrap_or(p.settings);
                if let Ok(s) = serde_json::from_value::<Settings>(v) {
                    // The client sends its configuration once at start-up too.
                    if s != self.settings {
                        log::info!("settings changed: {s:?}");
                        self.settings = s;
                        self.start_build();
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ requests

    /// Makes `a` the analysis requests use, with the next revision.
    fn install(&mut self, mut a: Analysis) {
        self.revision += 1;
        a.revision = self.revision;
        self.analysis = Arc::new(a);
    }

    fn spawn<P, R>(&mut self, req: Request, f: impl FnOnce(&Analysis, P) -> Result<R, String> + Send + 'static)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize,
    {
        self.flush_if_pending(&req.params);
        let id = req.id.clone();
        let params: P = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => {
                self.send(Message::Response(Response::new_err(id, ErrorCode::InvalidParams as i32, e.to_string())));
                return;
            }
        };
        let a = self.analysis.clone();
        let sender = self.sender.clone();
        let method = req.method.clone();
        rayon::spawn(move || {
            let t0 = Instant::now();
            let resp = match crash::catch(&method, Some(&sender), || f(&a, params)) {
                Some(Ok(v)) => Response::new_ok(id, v),
                Some(Err(msg)) => Response::new_err(id, ErrorCode::RequestFailed as i32, msg),
                None => Response::new_err(id, ErrorCode::InternalError as i32, format!("{method} failed")),
            };
            let ms = t0.elapsed().as_millis();
            if ms > 200 {
                log::info!("{method} took {ms} ms");
            }
            let _ = sender.send(Message::Response(resp));
        });
    }

    /// Requests on an open build or argument file: hover and links; nothing else applies.
    fn on_build_doc_request(&mut self, req: &Request) -> bool {
        if !req.method.starts_with("textDocument/") {
            return false;
        }
        let uri = req.params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| serde_json::from_value::<Url>(u.clone()).ok());
        let Some(key) = uri.as_ref().and_then(path_of).map(|p| url_of(&p)) else { return false };
        let Some(doc) = self.build_docs.get(&key) else { return false };
        let answers = matches!(req.method.as_str(), "textDocument/hover" | "textDocument/documentLink");
        let current = doc.inspected.as_ref().filter(|ins| ins.version == doc.version);
        if !answers || current.is_some() {
            let result = current.filter(|_| answers).map_or(Value::Null, |ins| build_docs::answer(&req.method, &req.params, ins));
            self.send(Message::Response(Response::new_ok(req.id.clone(), result)));
            return true;
        }
        // Edited since the last check: check this text now, so that ranges match it.
        let env = project_env(&self.settings);
        let job = self.inspect_job(&key, &env, 0);
        let (id, method, params, sender) = (req.id.clone(), req.method.clone(), req.params.clone(), self.sender.clone());
        rayon::spawn(move || {
            let result = job.and_then(|j| j()).map_or(Value::Null, |ins| build_docs::answer(&method, &params, &ins));
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        });
        true
    }

    fn on_request(&mut self, req: Request) {
        if self.on_build_doc_request(&req) {
            return;
        }
        match req.method.as_str() {
            "textDocument/hover" => self.spawn(req, |a, p: HoverParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(a.hover(f, off).map(|(md, span)| Hover {
                    contents: HoverContents::Markup(MarkupContent { kind: MarkupKind::Markdown, value: md }),
                    range: a.to_range(span),
                }))
            }),
            "textDocument/completion" => self.spawn(req, |a, p: CompletionParams| {
                let tp = p.text_document_position;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                let items = a.completions(f, off);
                Ok(Some(CompletionResponse::List(CompletionList { is_incomplete: items.len() >= 3000, items })))
            }),
            "textDocument/signatureHelp" => self.spawn(req, |a, p: SignatureHelpParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(a.signature_help(f, off))
            }),
            "textDocument/definition" | "textDocument/declaration" => self.spawn(req, |a, p: GotoDefinitionParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(GotoDefinitionResponse::Array(a.goto_definition(f, off))))
            }),
            "textDocument/typeDefinition" => self.spawn(req, |a, p: GotoDefinitionParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(GotoDefinitionResponse::Array(a.goto_type_definition(f, off))))
            }),
            "textDocument/implementation" => self.spawn(req, |a, p: GotoDefinitionParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(GotoDefinitionResponse::Array(a.goto_implementation(f, off))))
            }),
            "textDocument/references" => self.spawn(req, |a, p: ReferenceParams| {
                let tp = p.text_document_position;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(a.references(f, off, p.context.include_declaration)))
            }),
            "textDocument/documentHighlight" => self.spawn(req, |a, p: DocumentHighlightParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(
                    a.document_highlights(f, off)
                        .into_iter()
                        .filter(|s| s.file == f)
                        .filter_map(|s| a.to_range(s))
                        .map(|range| DocumentHighlight { range, kind: Some(DocumentHighlightKind::TEXT) })
                        .collect::<Vec<_>>(),
                ))
            }),
            "textDocument/documentSymbol" => self.spawn(req, |a, p: DocumentSymbolParams| {
                let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(None) };
                Ok(Some(DocumentSymbolResponse::Nested(a.document_symbols(f))))
            }),
            "workspace/symbol" => self.spawn(req, |a, p: WorkspaceSymbolParams| Ok(Some(WorkspaceSymbolResponse::Flat(a.workspace_symbols(&p.query, 500))))),
            "textDocument/prepareRename" => self.spawn(req, |a, p: TextDocumentPositionParams| {
                let Some((f, off)) = pos(a, &p.text_document.uri, p.position) else { return Ok(None) };
                Ok(a.prepare_rename(f, off).and_then(|s| a.to_range(s)).map(PrepareRenameResponse::Range))
            }),
            "textDocument/rename" => self.spawn(req, |a, p: RenameParams| {
                let tp = p.text_document_position;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                a.rename(f, off, &p.new_name).map(Some)
            }),
            "textDocument/semanticTokens/full" => self.spawn(req, |a, p: SemanticTokensParams| {
                let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(None) };
                Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data: a.semantic_tokens(f) })))
            }),
            "textDocument/foldingRange" => self.spawn(req, |a, p: FoldingRangeParams| {
                let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(None) };
                Ok(Some(a.folding_ranges(f)))
            }),
            "textDocument/prepareTypeHierarchy" => self.spawn(req, |a, p: TypeHierarchyPrepareParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(a.prepare_type_hierarchy(f, off)))
            }),
            "typeHierarchy/supertypes" | "typeHierarchy/subtypes" => {
                let sup = req.method == "typeHierarchy/supertypes";
                self.spawn(req, move |a, p: TypeHierarchySupertypesParams| {
                    let it = p.item;
                    let Some((f, off)) = pos(a, &it.uri, it.selection_range.start) else { return Ok(None) };
                    let Some(c) = a.class_at_location(f, off) else { return Ok(None) };
                    Ok(Some(if sup { a.supertypes(c) } else { a.subtypes(c) }))
                })
            }
            "textDocument/prepareCallHierarchy" => self.spawn(req, |a, p: CallHierarchyPrepareParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(a.prepare_call_hierarchy(f, off)))
            }),
            "callHierarchy/incomingCalls" => self.spawn(req, |a, p: CallHierarchyIncomingCallsParams| {
                let it = p.item;
                let Some((f, off)) = pos(a, &it.uri, it.selection_range.start) else { return Ok(None) };
                Ok(a.call_item_decl(f, off).map(|r| a.incoming_calls(r)))
            }),
            "callHierarchy/outgoingCalls" => self.spawn(req, |a, p: CallHierarchyOutgoingCallsParams| {
                let it = p.item;
                let Some((f, off)) = pos(a, &it.uri, it.selection_range.start) else { return Ok(None) };
                Ok(a.call_item_decl(f, off).map(|r| a.outgoing_calls(r)))
            }),
            // Links are for build files only (`on_build_doc_request`).
            "textDocument/documentLink" => self.send(Message::Response(Response::new_ok(req.id, Value::Null))),
            "textDocument/codeAction" => self.spawn(req, |a, p: CodeActionParams| {
                if p.context.only.as_ref().is_some_and(|o| !o.iter().any(|k| k.as_str().starts_with("quickfix"))) {
                    return Ok(None);
                }
                let Some((f, start)) = pos(a, &p.text_document.uri, p.range.start) else { return Ok(None) };
                let end = a.offset_at(f, p.range.end).unwrap_or(start);
                let actions: Vec<CodeActionOrCommand> = a
                    .quick_fixes(f, start, end)
                    .into_iter()
                    .filter_map(|q| {
                        let edit = a.versioned_edit(q.edits.clone())?;
                        Some(CodeActionOrCommand::CodeAction(CodeAction {
                            title: q.title,
                            kind: Some(CodeActionKind::QUICKFIX),
                            diagnostics: util::to_lsp_diag(a, &q.diag).map(|d| vec![d]),
                            edit: Some(edit),
                            is_preferred: q.preferred.then_some(true),
                            ..Default::default()
                        }))
                    })
                    .collect();
                Ok(Some(actions))
            }),
            // ---------------------------------------------------------- custom
            "dvicode/status" => self.spawn(req, |a, _: Value| Ok(a.status())),
            "dvicode/serverInfo" => self.spawn(req, |_, _: Value| {
                Ok(json!({
                    "version": crate::VERSION,
                    "pid": std::process::id(),
                    "host": fs::hostname(),
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "home": std::env::var("HOME").unwrap_or_default(),
                }))
            }),
            "dvicode/compiledFiles" => self.spawn(req, |a, _: Value| Ok(a.compiled_files())),
            "dvicode/designTops" => self.spawn(req, |a, _: Value| {
                Ok(a.design_tops().into_iter().map(|r| a.decl(r).name.to_string()).collect::<Vec<_>>())
            }),
            "dvicode/designHierarchy" => self.spawn(req, |a, p: Value| {
                let top = p.get("top").and_then(|t| t.as_str()).map(|s| s.to_string());
                Ok(a.design_hierarchy(top.as_deref(), 20000))
            }),
            "dvicode/verificationHierarchy" => self.spawn(req, |a, p: Value| {
                let test = p.get("test").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(|s| s.to_string());
                Ok(a.verification_hierarchy(test.as_deref(), 20000))
            }),
            // The views: one tree, filtered and limited (see TreeParams).
            "dvicode/tree" => self.spawn(req, |a, p: TreeParams| Ok(view_tree(a, p))),
            "dvicode/locate" => self.spawn(req, |a, p: LocateParams| {
                let located = pos(a, &p.text_document.uri, p.position).map(|(f, off)| a.locate(f, off)).unwrap_or_default();
                Ok(json!({ "revision": a.revision, "design": located.design, "instance": located.instance, "class": located.class }))
            }),
            "dvicode/structure" => self.spawn(req, |a, p: StructureParams| {
                use dvi_analysis::ide::filter::Filter;
                let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(json!({ "revision": a.revision, "status": "none", "nodes": [] })) };
                let filter = p.filter.as_deref().and_then(Filter::new);
                let t = a.structure(f, filter.as_ref(), p.max_nodes.unwrap_or(20_000).min(100_000));
                let version = a.source(f).and_then(|s| s.lsp_version);
                Ok(json!({ "revision": a.revision, "version": version, "status": t.status, "context": t.context, "others": t.others, "incomplete": t.incomplete, "nodes": t.nodes }))
            }),
            "dvicode/includingFiles" => self.spawn(req, |a, p: TextDocumentIdentifierParams| {
                let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(Vec::new()) };
                Ok(a.including_files(f))
            }),
            "dvicode/expandMacro" => self.spawn(req, |a, p: ExpandParams| {
                let Some((f, off)) = pos(a, &p.text_document.uri, p.position) else { return Ok(None) };
                let levels = if p.all.unwrap_or(false) { usize::MAX } else { 1 };
                Ok(a.expand_macro_at(f, off, levels).map(|t| json!({ "text": t })))
            }),
            "dvicode/buildConfigs" => {
                let root = self.root.clone();
                let current = self.analysis.setup.config.build_file.clone();
                self.spawn(req, move |_, _: Value| {
                    let files = root.as_deref().map(dvi_project::list_build_files).unwrap_or_default();
                    Ok(json!({
                        "current": current.map(|p| p.display().to_string()),
                        "available": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    }))
                })
            }
            // Fails on purpose, to test how internal errors reach the user (docs/development.md).
            "dvicode/debug/fail" => self.spawn(req, |_, _: Value| -> Result<Value, String> { panic!("test failure requested by the client") }),
            "dvicode/rebuild" => {
                // Rebuild Project reads every file again: some may have changed where no
                // watcher reports it (outside the workspace, or on a remote host).
                self.vfs.clear_disk_cache();
                self.start_build();
                self.send(Message::Response(Response::new_ok(req.id, Value::Null)));
            }
            m if m.starts_with("dvicode/fs/") => {
                let op = m.trim_start_matches("dvicode/fs/").to_string();
                let id = req.id.clone();
                let params = req.params;
                let sender = self.sender.clone();
                rayon::spawn(move || {
                    let feature = format!("remote file access ({op})");
                    let resp = match crash::catch(&feature, Some(&sender), || fs::handle(&op, params)) {
                        Some(Ok(v)) => Response::new_ok(id, v),
                        Some(Err(e)) => Response { id, result: None, error: Some(lsp_server::ResponseError { code: ErrorCode::RequestFailed as i32, message: e.message, data: Some(json!({ "code": e.code })) }) },
                        None => Response::new_err(id, ErrorCode::InternalError as i32, format!("{feature} failed")),
                    };
                    let _ = sender.send(Message::Response(resp));
                });
                return;
            }
            _ => {
                self.send(Message::Response(Response::new_err(req.id, ErrorCode::MethodNotFound as i32, format!("unhandled method {}", req.method))));
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextDocumentIdentifierParams {
    text_document: TextDocumentIdentifier,
}

/// `dvicode/tree`: the tree of a view (`design`, `verification` or `files`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TreeParams {
    view: String,
    /// Design Hierarchy: the top (default: the candidate tops).
    top: Option<String>,
    /// Verification Hierarchy: the test class, `*` for all (default: +UVM_TESTNAME).
    test: Option<String>,
    filter: Option<String>,
    /// Names from a root to a node returned even beyond the node limit.
    reveal: Option<Vec<String>>,
    /// Instead of a filter: the nodes of a design element, instance or class.
    target: Option<dvi_analysis::ide::views::Target>,
    max_nodes: Option<usize>,
    /// Verification Hierarchy: TLM ports are shown (default: yes, as older clients expect).
    ports: Option<bool>,
    /// Verification Hierarchy: objects (configurations, register models, sequences) are shown.
    objects: Option<bool>,
}

/// `dvicode/structure`: the Structure of a file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StructureParams {
    text_document: TextDocumentIdentifier,
    filter: Option<String>,
    max_nodes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocateParams {
    text_document: TextDocumentIdentifier,
    position: Position,
}

fn view_tree(a: &Analysis, p: TreeParams) -> Value {
    use dvi_analysis::ide::filter::Filter;
    use dvi_analysis::ide::views::TreeQuery;
    let mut q = TreeQuery::new(p.max_nodes.unwrap_or(20_000).min(100_000));
    q.filter = p.filter.as_deref().and_then(Filter::new);
    q.reveal = p.reveal.unwrap_or_default();
    q.target = p.target;
    q.ports = p.ports.unwrap_or(true);
    q.objects = p.objects.unwrap_or(false);
    if q.target.is_some() {
        // Every occurrence counts ("1 of N").
        q.max_matches = q.max_nodes;
    }
    let nonempty = |s: Option<String>| s.filter(|s| !s.is_empty());
    match p.view.as_str() {
        "design" => {
            let (roots, meta) = a.design_tree(nonempty(p.top).as_deref(), &q);
            json!({ "revision": a.revision, "incomplete": meta.incomplete, "matches": meta.matches, "roots": roots })
        }
        "verification" => {
            let (h, meta) = a.verification_tree(nonempty(p.test).as_deref(), &q);
            json!({ "revision": a.revision, "incomplete": meta.incomplete, "matches": meta.matches, "roots": h.roots, "test": h.test, "configured": h.configured, "tests": h.tests })
        }
        _ => {
            let (roots, meta) = a.compiled_files_tree(&q);
            json!({ "revision": a.revision, "incomplete": meta.incomplete, "matches": meta.matches, "roots": roots })
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpandParams {
    text_document: TextDocumentIdentifier,
    position: Position,
    all: Option<bool>,
}

/// The project configuration the settings ask for: a build file, the default one, or
/// auto-configuration (reported on and cancelled through `progress`).
fn project_setup(root: Option<&std::path::Path>, settings: &Settings, progress: &Progress) -> ProjectSetup {
    let base = root.map(|r| r.to_path_buf()).unwrap_or_else(|| PathBuf::from("/"));
    let env = project_env(settings);
    let wanted = settings.build_file.as_deref().map(str::trim).unwrap_or("");
    if wanted == dvi_project::AUTO {
        let cfg = if root.is_some() { dvi_project::load_auto_with(&base, &env, progress) } else { Default::default() };
        return ProjectSetup::new(&base, cfg, env);
    }
    let bf = Some(wanted).filter(|b| !b.is_empty()).map(|b| {
        let p = PathBuf::from(b);
        if p.is_absolute() {
            p
        } else {
            base.join(p)
        }
    });
    let cfg = if root.is_some() { dvi_project::load_with(&base, bf.as_deref(), &env, progress) } else { Default::default() };
    ProjectSetup::new(&base, cfg, env)
}

/// The variables build files are read with: `dvicode.env`, and `dvicode.uvmHome`.
fn project_env(settings: &Settings) -> FxHashMap<String, String> {
    let mut env = settings.env.clone();
    if let Some(u) = &settings.uvm_home {
        env.insert("DVICODE_UVM_HOME".into(), u.clone());
    }
    env
}

/// "build file .dvicode/default.build: 12 top files" or "auto-configuration of D:\proj: ...".
fn describe_config(setup: &ProjectSetup) -> String {
    let tops: usize = setup.config.invocations.iter().map(|i| i.files.len()).sum();
    let what = match &setup.config.build_file {
        Some(bf) => format!("build file {}", bf.strip_prefix(&setup.root).unwrap_or(bf).display()),
        None => format!("auto-configuration of {}", setup.root.display()),
    };
    format!("{what}: {} invocation(s), {tops} top files", setup.config.invocations.len())
}

/// A SystemVerilog or Verilog source or header, by extension.
fn is_sv_file(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "sv" | "svh" | "svi" | "svp" | "sva" | "v" | "vh" | "vp"))
}

fn pos(a: &Analysis, uri: &Url, p: Position) -> Option<(FileId, u32)> {
    let f = a.file_of_url(uri)?;
    Some((f, a.offset_at(f, p)?))
}

#[cfg(test)]
mod tests;
