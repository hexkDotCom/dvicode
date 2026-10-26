//! The LSP main loop.

mod build_docs;

use crate::{crash, format, fs, util};
use build_docs::{BuildDoc, Inspected};
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use dvi_analysis::ide::hints::HintKinds;
use dvi_analysis::ide::tokens::{TOKEN_MODIFIERS, TOKEN_TYPES};
use dvi_analysis::ide::{path_of, url_of};
use dvi_analysis::{Analysis, Pin, ProjectSetup, UnitKind, Vfs};
use dvi_project::Progress;
use dvi_syntax::{FileId, SourceProvider};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Res<T> = Result<T, Box<dyn Error + Sync + Send>>;

const DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Build configuration file, relative to the workspace root or absolute.
    #[serde(deserialize_with = "lenient")]
    pub build_file: Option<String>,
    /// Extra environment variables for build file expansion.
    #[serde(deserialize_with = "lenient")]
    pub env: FxHashMap<String, String>,
    /// UVM library location used by `-uvm` and auto-configuration.
    #[serde(deserialize_with = "lenient")]
    pub uvm_home: Option<String>,
    /// Maximum number of problems published after a full build (0 = no limit).
    #[serde(deserialize_with = "lenient")]
    pub max_problems: Option<usize>,
}

/// A setting of the wrong type (`-1` for a count: VS Code does not check settings against
/// their schema) keeps its default instead of failing, and so resetting, every setting.
fn lenient<'de, D: serde::Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
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
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX, CodeActionKind::REFACTOR_REWRITE]),
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        })),
        document_link_provider: Some(DocumentLinkOptions { resolve_provider: Some(false), work_done_progress_options: Default::default() }),
        inlay_hint_provider: Some(OneOf::Left(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
            legend: SemanticTokensLegend { token_types: TOKEN_TYPES.to_vec(), token_modifiers: TOKEN_MODIFIERS.to_vec() },
            full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
            range: None,
            work_done_progress_options: Default::default(),
        })),
        // What the views can ask for, so a newer extension with an older engine shows only what
        // it supports.
        experimental: Some(json!({ "dvicode": { "verificationObjects": 1, "structure": 1, "registers": 1, "checks": 1, "accesses": 1, "compileContext": 1 } })),
        ..Default::default()
    }
}

/// What has been published, and which publishing job may publish each file.
#[derive(Default)]
struct PubState {
    /// Files that currently show problems in the client, and how many.
    published: FxHashMap<Url, usize>,
    /// File -> sequence number of the newest job that covers it.
    latest: FxHashMap<Url, u64>,
    next: u64,
}

/// `dvicode.inlayHints` in the settings `v` (the `dvicode` section): each kind on unless set
/// to `false`.
fn hint_kinds_of(v: &Value) -> HintKinds {
    let h = v.get("inlayHints");
    let on = |k: &str| h.and_then(|h| h.get(k)).and_then(Value::as_bool).unwrap_or(true);
    HintKinds { ports: on("ports"), parameters: on("parameters"), arguments: on("arguments") }
}

/// The semantic tokens last answered for each file: answered again for the same snapshot,
/// and what a `full/delta` answer for a later one tells the changes from.
#[derive(Default)]
struct TokenResults {
    by_file: FxHashMap<FileId, TokenResult>,
    /// The open documents: only theirs are kept (an answer finished after a close is not).
    open: FxHashSet<FileId>,
    next: u64,
}

struct TokenResult {
    revision: u64,
    id: String,
    data: Arc<Vec<SemanticToken>>,
}

type Tokens = (String, Arc<Vec<SemanticToken>>);

impl TokenResults {
    /// The tokens of `f` in the snapshot of `a`, and those answered before for another
    /// snapshot, if any.
    fn get(results: &Mutex<TokenResults>, a: &Analysis, f: FileId) -> (Tokens, Option<Tokens>) {
        let before = results.lock().unwrap().by_file.get(&f).map(|r| (r.revision, r.id.clone(), r.data.clone()));
        if let Some((rev, id, data)) = &before {
            if *rev == a.revision {
                return ((id.clone(), data.clone()), None);
            }
        }
        let data = Arc::new(a.semantic_tokens(f));
        let mut g = results.lock().unwrap();
        g.next += 1;
        let id = g.next.to_string();
        // Kept unless a newer snapshot's were kept meanwhile, or the document was closed.
        if g.open.contains(&f) && g.by_file.get(&f).is_none_or(|r| r.revision < a.revision) {
            g.by_file.insert(f, TokenResult { revision: a.revision, id: id.clone(), data: data.clone() });
        }
        ((id, data), before.map(|(_, id, data)| (id, data)))
    }
}

/// The change from `old` to `new` as one edit of the client's array (five numbers a token):
/// what comes between their common start and their common end. None when they are the same.
fn token_edits(old: &[SemanticToken], new: &[SemanticToken]) -> Vec<SemanticTokensEdit> {
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let end = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    if start == old.len() && start == new.len() {
        return Vec::new();
    }
    vec![SemanticTokensEdit { start: (start * 5) as u32, delete_count: ((old.len() - start - end) * 5) as u32, data: Some(new[start..new.len() - end].to_vec()) }]
}

enum Event {
    /// A finished full build (`None`: cancelled for a newer one); the error message when it
    /// failed and fell back to an empty project.
    Built(Option<Box<Analysis>>, u64, Option<String>),
    /// An open build or argument file checked: its URI and version, and what was found.
    Inspected(Url, u64, Inspected),
    /// The update `UpdateJob::seq` finished; `None` when it failed.
    Updated(u64, Option<Box<Updated>>),
    /// Inputs of the build of that generation changed since it read them.
    InputsChanged(u64, Vec<PathBuf>),
}

/// Edits to apply to a snapshot, on the update thread (`update_thread`).
#[derive(Clone)]
struct UpdateJob {
    seq: u64,
    /// The snapshot in use when it started.
    base: Arc<Analysis>,
    /// The edited files, sorted.
    files: Vec<FileId>,
    /// The open documents, in a stable order.
    open: Vec<FileId>,
    /// The editor texts it compiles: those of the moment it started.
    pin: Arc<Pin>,
    /// Cancelled when a newer job replaces it.
    progress: Arc<Progress>,
}

/// What an update made.
struct Updated {
    analysis: Analysis,
    /// Declarations or macros other files use changed.
    interface_changed: bool,
    /// Files of the units compiled again, before and after.
    recompiled: Vec<FileId>,
}

/// See [`Server::line_local`]: the place of the cursor in the snapshot in use, and the editor's
/// text of its line before it.
struct LineLocal {
    file: FileId,
    /// Where the line starts in the snapshot's text (and where its scope is looked up).
    off: u32,
    line: String,
    /// The editor's text it answers for: when it has changed by the time the answer is made,
    /// there is none (as for a waiting request whose document changes).
    text: Arc<str>,
}

impl LineLocal {
    fn current(&self, a: &Analysis) -> bool {
        a.vfs.text(self.file).is_some_and(|t| Arc::ptr_eq(&t, &self.text))
    }
}

/// A request that waits for edits to be analysed (see `Server::spawn`).
struct Deferred {
    id: RequestId,
    /// The last edit made before it came (`Server::edit_seq`): it waits for edits up to that
    /// one, never for later ones.
    seq: u64,
    /// It looks at the whole project, so it waits for every edit; otherwise for the edits of
    /// files compiled with `file` or before it (`Server::needs`).
    project: bool,
    /// The file it is about (`None`: none, it waits for every edit).
    file: Option<FileId>,
    /// The files with edits it needed when it came (compiled with `file` then): an update
    /// in between that separates them does not make it go without them.
    needed: Vec<FileId>,
    /// It names a position or range in the file, which a later edit makes meaningless.
    positional: bool,
    /// Answers it from a snapshot.
    run: Box<dyn FnOnce(Arc<Analysis>) + Send>,
}

/// Lets tests hold the update thread.
#[derive(Default)]
pub(crate) struct Hooks {
    /// Called on the update thread before each update, and after it (before it is handed
    /// to the loop).
    pub before_update: Option<Box<dyn Fn() + Send + Sync>>,
    pub after_update: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called on the build thread when a full build is done, before it is handed to the loop.
    pub after_build: Option<Box<dyn Fn() + Send + Sync>>,
    /// Called on a worker thread when a request's handler starts (not cancelled before it
    /// could), with the method.
    pub before_request: Option<Box<dyn Fn(&str) + Send + Sync>>,
    /// Called when a problem check starts (true, on the loop) and when it is done (false).
    pub problem_check: Option<Box<dyn Fn(bool) + Send + Sync>>,
    /// Called for each file of a problem check: the check's number, the file, and whether its
    /// problems are computed (not when a newer check covers the file).
    pub before_problems: Option<Box<dyn Fn(u64, &std::path::Path, bool) + Send + Sync>>,
}

/// The requests being answered, and the flags that cancel them.
type Running = Arc<Mutex<FxHashMap<RequestId, Arc<AtomicBool>>>>;

struct Server {
    sender: Sender<Message>,
    vfs: Arc<Vfs>,
    analysis: Arc<Analysis>,
    root: Option<PathBuf>,
    settings: Settings,
    /// Which inlay hints to give (`dvicode.inlayHints`): no build depends on them.
    hint_kinds: HintKinds,
    /// The `dvicode.format` settings: no build depends on them.
    format_settings: Value,
    /// The client can be asked to request inlay hints again.
    hint_refresh: bool,
    hints_refreshed: Option<Instant>,
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
    /// The build file of the last full build announced (`None` before the first).
    announced_build_file: Option<Option<PathBuf>>,
    published: Arc<Mutex<PubState>>,
    token_results: Arc<Mutex<TokenResults>>,
    register_models: RegisterModels,
    next_id: i32,
    /// The client shows `$/progress` notifications.
    progress: bool,
    /// The client watches files the server names (dynamic registration of
    /// `workspace/didChangeWatchedFiles`).
    watch_inputs: bool,
    /// It takes patterns relative to a folder (else absolute ones are sent).
    relative_patterns: bool,
    /// The build inputs it is watching for the server.
    watched: Vec<PathBuf>,
    /// The registration request awaiting its answer.
    watch_request: Option<RequestId>,
    /// Files changed while a build ran: one the new build turns out to read makes it out of
    /// date (it may have read the old text, and no watcher saw the change).
    changed_while_building: FxHashSet<PathBuf>,
    /// Progress of the running build, and the switch that cancels it.
    build_progress: Option<Arc<Progress>>,
    build_started: Instant,
    /// What the client was last told (message, percentage), and when progress was last logged.
    reported: (String, u32),
    logged_at: Instant,
    /// Build and argument files open in the editor, and when to check them again.
    build_docs: FxHashMap<Url, BuildDoc>,
    inspect_at: Option<Instant>,
    /// A hint refresh held back by the once-a-second limit: sent when it allows.
    hints_due: Option<Instant>,
    /// The client applies workspace edits given as `documentChanges` (else as `changes`).
    document_changes: bool,
    /// The request creating the build's progress token, until the client answers it.
    progress_request: Option<RequestId>,
    /// The client accepted the build's progress token: progress may be reported on it.
    progress_live: bool,
    /// Numbers the checks of build documents (`BuildDoc::seq`).
    inspect_seq: u64,
    /// The update thread's jobs, the one running (or queued), and the last job number.
    update_tx: Sender<UpdateJob>,
    update: Option<UpdateJob>,
    update_seq: u64,
    /// Requests waiting for the running update, in arrival order.
    deferred: Vec<Deferred>,
    /// Requests handed to a handler (waiting or not), until answered.
    running: Running,
    hooks: Arc<Hooks>,
    /// A finished full build, installed when the update thread has applied the edits made
    /// while it ran (until then requests use the previous snapshot, which has them).
    built: Option<Arc<Analysis>>,
    /// A full build is announced (status, all problems) once installed; the error message
    /// when it failed.
    announce: Option<Option<String>>,
    /// Files changed on disk while an update or a build waiting for one ran, that the snapshot
    /// in use does not compile: the update may have read them (see `on_updated`).
    changed_while_updating: FxHashSet<FileId>,
    /// Files opened while an update or a build ran, which did not see them (see `on_updated`):
    /// the number of the edit that opened each, and whether it was looked for since.
    opened_while_updating: FxHashMap<FileId, (u64, bool)>,
    /// Numbers the edits (`note_change`).
    edit_seq: u64,
    /// Files with edits the snapshot in use does not have yet: the number of the first.
    dirty: FxHashMap<FileId, u64>,
    /// Files of the running update edited again after it started: the number of the first
    /// such edit (dirty again when it is installed).
    redirty: FxHashMap<FileId, u64>,
}

/// How long typing in a build file pauses before it is checked.
const INSPECT_DELAY: Duration = Duration::from_millis(300);

/// How often the running build's progress is sent to the client, and written to the log.
const REPORT_EVERY: Duration = Duration::from_millis(500);
const LOG_EVERY: Duration = Duration::from_secs(5);

pub fn run(conn: Connection) -> Res<()> {
    run_with(conn, Hooks::default())
}

pub(crate) fn run_with(conn: Connection, hooks: Hooks) -> Res<()> {
    let (id, params) = conn.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let result = InitializeResult {
        capabilities: capabilities(),
        server_info: Some(ServerInfo { name: "dvicode-server".into(), version: Some(crate::VERSION.into()) }),
    };
    let mut result = serde_json::to_value(result)?;
    // Not in this lsp-types' ServerCapabilities; clients register the feature only when told.
    result["capabilities"]["typeHierarchyProvider"] = json!(true);
    conn.initialize_finish(id, result)?;

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
    // The network hosts paths may name (VS Code's `security.allowedUNCHosts`, when it restricts
    // them): read here only, not with the settings that change, as the editor sends it once.
    // Anything but a list or `null` (no restriction) allows no host; a name that is not a string
    // names none.
    let unc_hosts: Option<Vec<String>> = match params.initialization_options.as_ref().and_then(|v| v.get("uncHosts")) {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect()),
        Some(_) => Some(Vec::new()),
    };
    if let Some(h) = &unc_hosts {
        log::info!("network hosts allowed: {h:?}");
    }
    dvi_project::netpath::set_allowed_hosts(unc_hosts);
    let progress = params.capabilities.window.as_ref().and_then(|w| w.work_done_progress).unwrap_or(false);
    let watched_files = params.capabilities.workspace.as_ref().and_then(|w| w.did_change_watched_files.as_ref());
    let watch_inputs = watched_files.and_then(|d| d.dynamic_registration).unwrap_or(false);
    let relative_patterns = watched_files.and_then(|d| d.relative_pattern_support).unwrap_or(false);
    let hint_kinds = params.initialization_options.as_ref().map(hint_kinds_of).unwrap_or_default();
    let format_settings = params.initialization_options.as_ref().and_then(|v| v.get("format")).cloned().unwrap_or(Value::Null);
    let hint_refresh = params.capabilities.workspace.as_ref().and_then(|w| w.inlay_hint.as_ref()).and_then(|h| h.refresh_support).unwrap_or(false);
    let document_changes = params.capabilities.workspace.as_ref().and_then(|w| w.workspace_edit.as_ref()).and_then(|e| e.document_changes).unwrap_or(false);
    log::info!("workspace root: {:?}; settings: {:?}", root, settings);
    let vfs = Arc::new(Vfs::new());
    let empty_setup = Arc::new(ProjectSetup::new(root.as_deref().unwrap_or(std::path::Path::new("/")), Default::default(), Default::default()));
    let analysis = Arc::new(Analysis::build(vfs.clone(), empty_setup));
    let (events_tx, events_rx) = unbounded();
    let (update_tx, update_rx) = unbounded();
    let hooks = Arc::new(hooks);
    {
        let (events, client, hooks) = (events_tx.clone(), conn.sender.clone(), hooks.clone());
        std::thread::Builder::new()
            .name("dvicode-update".into())
            .stack_size(64 << 20)
            .spawn(move || update_thread(update_rx, events, client, hooks))
            .expect("spawn update thread");
    }
    let mut s = Server {
        sender: conn.sender.clone(),
        vfs,
        analysis,
        root,
        settings,
        hint_kinds,
        format_settings,
        hint_refresh,
        hints_refreshed: None,
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
        announced_build_file: None,
        published: Arc::new(Mutex::new(PubState::default())),
        token_results: Arc::default(),
        register_models: Arc::default(),
        next_id: 1,
        progress,
        watch_inputs,
        relative_patterns,
        watched: Vec::new(),
        watch_request: None,
        changed_while_building: FxHashSet::default(),
        build_progress: None,
        build_started: Instant::now(),
        reported: (String::new(), 0),
        logged_at: Instant::now(),
        build_docs: FxHashMap::default(),
        inspect_at: None,
        hints_due: None,
        progress_request: None,
        progress_live: false,
        document_changes,
        inspect_seq: 0,
        update_tx,
        update: None,
        update_seq: 0,
        deferred: Vec::new(),
        running: Running::default(),
        hooks,
        built: None,
        announce: None,
        changed_while_updating: FxHashSet::default(),
        opened_while_updating: FxHashMap::default(),
        edit_seq: 0,
        dirty: FxHashMap::default(),
        redirty: FxHashMap::default(),
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
            if let Some(t) = self.hints_due {
                timeout = timeout.min(t.saturating_duration_since(Instant::now()));
            }
            select! {
                recv(conn.receiver) -> msg => {
                    let Ok(msg) = msg else {
                        self.stop_work();
                        return Ok(());
                    };
                    match msg {
                        Message::Request(req) => {
                            if req.method == "shutdown" {
                                self.stop_work();
                            }
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
                                self.stop_work();
                                return Ok(());
                            }
                            let method = n.method.clone();
                            let tx = self.sender.clone();
                            crash::catch(&method, Some(&tx), || self.on_notification(n));
                        }
                        Message::Response(r) => {
                            if self.progress_request.as_ref() == Some(&r.id) {
                                self.progress_request = None;
                                if r.error.is_none() && self.building {
                                    self.progress_live = true;
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
                            } else if r.error.is_some() && self.watch_request.as_ref() == Some(&r.id) {
                                log::warn!("the editor did not take the watchers of the build's files: {:?}", r.error);
                                self.watch_inputs = false;
                                self.watched.clear();
                            } else if self.watch_request.as_ref() == Some(&r.id) {
                                // Watched from now on: what changed before is looked for on disk.
                                self.check_inputs(self.analysis.setup.clone());
                            }
                        }
                    }
                }
                recv(self.events_rx) -> ev => {
                    match ev {
                        Ok(Event::Built(a, generation, error)) => self.on_built(a.map(|a| *a), generation, error),
                        Ok(Event::Inspected(uri, seq, ins)) => self.on_inspected(uri, seq, ins),
                        Ok(Event::Updated(seq, done)) => self.on_updated(seq, done.map(|u| *u)),
                        Ok(Event::InputsChanged(generation, paths)) => {
                            // A build started since then reads them again.
                            if generation == self.build_generation && !self.building {
                                log::info!("build again: changed while nothing watched them: {}", paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
                                self.rebuild_at = Some(Instant::now() + Duration::from_millis(500));
                            }
                        }
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
            if self.hints_due.is_some_and(|t| Instant::now() >= t) {
                self.hints_due = None;
                self.refresh_hints(true);
            }
        }
    }

    /// At shutdown: the running update and build stop, so that their threads let go of the
    /// connection (a TCP transport waits for them before the process exits).
    fn stop_work(&mut self) {
        if let Some(job) = self.update.take() {
            job.progress.cancel();
        }
        if let Some(p) = &self.build_progress {
            p.cancel();
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
            st.published.insert(uri.clone(), diagnostics.len());
        }
        let p = PublishDiagnosticsParams { uri, diagnostics, version: Some(version) };
        let _ = self.sender.send(Message::Notification(Notification::new("textDocument/publishDiagnostics".into(), p)));
    }

    /// Sends the running build's progress to the client when it changed, and logs it now and
    /// then, so that a long build shows what it is doing.
    fn report_build_progress(&mut self) {
        let Some(p) = self.build_progress.clone() else { return };
        let (message, percentage) = (p.message(), p.percentage());
        if self.progress_live && (self.reported.0 != message || self.reported.1 != percentage) {
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

    fn request_client(&mut self, method: &str, params: impl Serialize) -> RequestId {
        let id = RequestId::from(format!("dvicode-{}", self.next_id));
        self.next_id += 1;
        self.send(Message::Request(Request::new(id.clone(), method.into(), params)));
        id
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
        self.changed_while_building.clear();
        // Folders may have become links since the last build.
        dvi_project::fsx::forget();
        self.build_generation += 1;
        let generation = self.build_generation;
        let progress = Arc::new(Progress::new());
        self.build_progress = Some(progress.clone());
        self.build_started = Instant::now();
        self.logged_at = Instant::now();
        self.reported = (String::new(), 0);
        if self.progress {
            // The progress begins once the client has accepted the token.
            self.progress_live = false;
            let id = self.request_client("window/workDoneProgress/create", WorkDoneProgressCreateParams { token: NumberOrString::String("dvicode-build".into()) });
            self.progress_request = Some(id);
        }
        let (root, settings) = (self.root.clone(), self.settings.clone());
        let vfs = self.vfs.clone();
        let tx = self.events_tx.clone();
        let client = self.sender.clone();
        let hooks = self.hooks.clone();
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
                if let Some(h) = &hooks.after_build {
                    h();
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
        // A file it reads changed while it ran: build again. Told by the client, or seen on
        // disk for a file nothing watched yet (an argument file of the first build; looked
        // for again once the client watches it).
        let changed = std::mem::take(&mut self.changed_while_building);
        if changed.iter().any(|c| a.setup.config.inputs.iter().any(|p| build_docs::same_file(p, c))) {
            self.rebuild_at = Some(Instant::now() + Duration::from_millis(500));
        }
        self.check_inputs(a.setup.clone());
        // A running update started from the snapshot this replaces (or from an older build).
        self.cancel_update();
        // Files opened while it ran are looked for again in it once it is in.
        for (_, done) in self.opened_while_updating.values_mut() {
            *done = false;
        }
        // The update thread applies the edits made while the build ran, including those
        // already applied to the previous snapshot by the debounce (the build may have read
        // older text), and compiles open files that are not part of the build ad hoc.
        self.pending.extend(self.changed_during_build.drain());
        for f in self.open.keys() {
            if a.units_of_file(*f).is_empty() {
                self.pending.insert(*f);
            }
        }
        self.end_progress();
        self.announce = Some(error);
        if self.pending.is_empty() {
            self.built = None;
            self.install(a);
            self.announce_build();
        } else {
            self.built = Some(Arc::new(a));
            self.flush_pending();
        }
        if self.build_again {
            self.build_again = false;
            self.start_build();
        }
        self.run_deferred();
    }

    /// Looks on another thread (it asks the file system about every input) for inputs of the
    /// current build's setup that changed since the build read them.
    fn check_inputs(&self, setup: Arc<ProjectSetup>) {
        let (tx, generation) = (self.events_tx.clone(), self.build_generation);
        std::thread::spawn(move || {
            let changed: Vec<PathBuf> = setup.config.changed_inputs().into_iter().filter(|p| !is_log(p)).map(Path::to_path_buf).collect();
            if !changed.is_empty() {
                let _ = tx.send(Event::InputsChanged(generation, changed));
            }
        });
    }

    /// Tells the client about the full build installed last: its status, and all problems.
    fn announce_build(&mut self) {
        let Some(error) = self.announce.take() else { return };
        // Tell when the build file in use changed (one added, removed or chosen).
        let bf = self.analysis.setup.config.build_file.clone();
        if self.announced_build_file.as_ref().is_some_and(|old| *old != bf) {
            let text = match &bf {
                Some(bf) => {
                    let shown = self.root.as_deref().and_then(|r| bf.strip_prefix(r).ok()).unwrap_or(bf);
                    format!("DVICode now uses the build file {}", shown.display())
                }
                None => "DVICode now configures the project automatically (no build file)".to_string(),
            };
            log::info!("{text}");
            self.notify("window/showMessage", ShowMessageParams { typ: MessageType::INFO, message: text });
        }
        self.announced_build_file = Some(bf);
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
        // The files the build no longer compiles (another configuration, a branch switch)
        // keep no text.
        let a = self.analysis.clone();
        self.vfs.forget_texts_except(|id| !a.units_of_file(id).is_empty());
        self.publish_all();
        // The build may read open argument files differently now.
        if !self.build_docs.is_empty() {
            self.inspect_at = Some(Instant::now());
        }
    }

    fn end_progress(&mut self) {
        // Ended only on a token the client accepted (a build may end before it answers).
        self.progress_request = None;
        if std::mem::take(&mut self.progress_live) {
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
        self.drop_deferred(&[], Some(id));
        self.compile_again(id);
    }

    /// A new header not saved yet, or one opened while an update that looked for it ran: the
    /// files whose `include found no file of its name are compiled again; with `opened`, as
    /// part of that edit (requests sent after it wait for them).
    fn retry_includes_of(&mut self, id: FileId, opened: Option<u64>) {
        let path = self.vfs.path(id);
        let name = path.file_name();
        let roots: Vec<FileId> = self
            .analysis
            .units
            .iter()
            .flatten()
            .filter(|u| u.includes.iter().any(|i| i.resolved.is_none() && std::path::Path::new(&i.path_text).file_name() == name))
            .map(|u| u.root)
            .collect();
        for r in roots {
            if let Some(seq) = opened {
                let first = self.dirty.entry(r).or_insert(seq);
                *first = (*first).min(seq);
            }
            self.compile_again(r);
        }
    }

    /// Has the units of `id` compiled again, as after an edit of it (its text is the same:
    /// requests about positions in it stay valid).
    fn compile_again(&mut self, id: FileId) {
        self.number_edit(id);
        self.pending.insert(id);
        self.pending_since.get_or_insert_with(Instant::now);
        if self.building {
            self.changed_during_build.insert(id);
        }
    }

    /// Gives an edit of `id` its number: requests that come later do not wait for it, those
    /// before do (an edit made while the running update compiles the file is not in it).
    fn number_edit(&mut self, id: FileId) {
        self.edit_seq += 1;
        self.dirty.entry(id).or_insert(self.edit_seq);
        if self.update.as_ref().is_some_and(|j| j.files.binary_search(&id).is_ok()) {
            self.redirty.entry(id).or_insert(self.edit_seq);
        }
    }

    /// Hands the pending edits to the update thread. An update that is running already goes
    /// on unless a file it compiles was edited again: then one update with the files of both
    /// replaces it, unless a waiting request needs it (one request after another could then
    /// wait for ever). Other edits wait for it.
    fn flush_pending(&mut self) {
        if let Some(job) = &self.update {
            let again = self.pending.iter().any(|f| job.files.binary_search(f).is_ok());
            if !again || self.deferred.iter().any(|d| job.files.iter().any(|f| self.needs(d, *f))) {
                // Flushed when it is done (`on_updated`).
                self.pending_since = None;
                return;
            }
            self.cancel_update();
        }
        self.pending_since = None;
        if self.pending.is_empty() {
            return;
        }
        let mut files: Vec<FileId> = self.pending.drain().collect();
        files.sort_unstable();
        self.update_seq += 1;
        let base = self.built.clone().unwrap_or_else(|| self.analysis.clone());
        let pin = Arc::new(self.vfs.pin());
        let job = UpdateJob { seq: self.update_seq, base, files, open: self.open_files(), pin, progress: Arc::new(Progress::new()) };
        let _ = self.update_tx.send(job.clone());
        self.update = Some(job);
        self.redirty.clear();
    }

    /// Stops the running update (its snapshot is being replaced): its files are pending again.
    fn cancel_update(&mut self) {
        if let Some(job) = self.update.take() {
            job.progress.cancel();
            self.pending.extend(job.files);
            self.pending_since.get_or_insert_with(Instant::now);
        }
    }

    fn on_updated(&mut self, seq: u64, done: Option<Updated>) {
        if self.update.as_ref().map_or(true, |j| j.seq != seq) {
            return; // replaced by a newer job
        }
        let Some(job) = self.update.take() else { return };
        let mut interface_changed = false;
        let mut recompiled = Vec::new();
        let base = self.built.as_ref().unwrap_or(&self.analysis);
        match done {
            Some(_) if !Arc::ptr_eq(&job.base, base) => {
                // Started from a snapshot that was replaced since (replacing one cancels the
                // update, so this is a safety net): again, from the one in use.
                log::warn!("update of {} file(s) started from an older snapshot; updating again", job.files.len());
                self.pending.extend(job.files);
                self.flush_pending();
                return;
            }
            Some(u) => {
                interface_changed = u.interface_changed;
                recompiled = u.recompiled;
                self.built = None;
                self.install(u.analysis);
                // The views refresh what they show.
                self.notify("dvicode/revision", &json!({ "revision": self.revision }));
            }
            None => {
                log::error!("kept the previous analysis of {:?}", job.files.iter().map(|f| self.vfs.path(*f)).collect::<Vec<_>>());
                // A finished build is used without the edits.
                if let Some(b) = self.built.take() {
                    self.install((*b).clone());
                }
            }
        }
        // Its edits are in (or given up on, when it failed); those made after it started are not.
        for f in &job.files {
            match self.redirty.remove(f) {
                Some(first) => self.dirty.insert(*f, first),
                None => self.dirty.remove(f),
            };
        }
        self.redirty.clear();
        // Files changed on disk while it ran that it may have read (an edit added an
        // `include of them): they are compiled again; requests sent since wait for that.
        for f in std::mem::take(&mut self.changed_while_updating) {
            if !self.analysis.units_of_file(f).is_empty() {
                // Keeps the number it was given when the change was reported.
                self.note_change(f);
            } else if !self.pending.contains(&f) {
                self.dirty.remove(&f);
            }
        }
        // Files opened while it ran, whose name an include it compiled may have looked for;
        // kept while a build or an update that may not have seen them either is still to come.
        // Once each (an include that does not reach it would be tried again and again).
        let opened: Vec<(FileId, u64)> = self.opened_while_updating.iter_mut().filter(|(_, (_, done))| !*done).map(|(f, (s, done))| {
            *done = true;
            (*f, *s)
        }).collect();
        for (f, seq) in opened {
            if self.open.contains_key(&f) {
                self.retry_includes_of(f, Some(seq));
            }
        }
        if self.built.is_none() && !self.building {
            self.opened_while_updating.clear();
        }
        if self.announce.is_some() {
            self.announce_build();
        } else if interface_changed {
            // Declarations or macros other files use changed: their problems may have too.
            self.publish_all();
        } else {
            // Problems of the changed files, of the other files compiled again with them, and
            // of the open files.
            let mut targets: FxHashSet<FileId> = job.files.iter().copied().collect();
            targets.extend(recompiled);
            for f in self.open.keys() {
                targets.insert(*f);
            }
            // Their SKIPPED_CODE notices count uses in every file (no build file).
            targets.extend(self.analysis.files_with_skipped_code());
            self.publish(targets.into_iter().collect(), false);
        }
        self.run_deferred();
        // Edits that waited for this update: their pause in typing is over, or a request
        // waits for them (typing elsewhere must not hold it up).
        if self.pending_since.is_none() || !self.deferred.is_empty() {
            self.flush_pending();
        }
    }

    /// Whether request `d` needs the edits of file `f` that are not analysed yet: those made
    /// before it came, of `f` itself, of a file compiled with it (a header and the files that
    /// include it) or before it in the same build invocation (whose macros it is compiled
    /// with), or of any file for a request about the whole project.
    fn needs(&self, d: &Deferred, f: FileId) -> bool {
        self.dirty.get(&f).is_some_and(|first| *first <= d.seq) && (d.needed.contains(&f) || self.related(d.project, d.file, f))
    }

    fn related(&self, project: bool, file: Option<FileId>, f: FileId) -> bool {
        let Some(file) = file.filter(|_| !project) else { return true };
        if file == f {
            return true;
        }
        let units = self.analysis.units_of_file(file);
        !units.is_empty() && (self.analysis.units_of_file(f).iter().any(|u| units.contains(u)) || self.analysis.macros_flow(f, file))
    }

    /// Whether `d` must wait for edits not analysed yet.
    fn waits(&self, d: &Deferred) -> bool {
        self.dirty.keys().any(|f| self.needs(d, *f))
    }

    /// Answers the waiting requests whose edits are analysed now.
    fn run_deferred(&mut self) {
        for d in std::mem::take(&mut self.deferred) {
            if self.waits(&d) {
                self.deferred.push(d);
            } else {
                (d.run)(self.analysis.clone());
            }
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
            if full {
                // The files it does not cover (no longer compiled) are its too, to clear: an
                // older job still running must not publish them after it.
                for (u, owner) in st.latest.iter_mut() {
                    if *owner < g && !skip.contains(u) {
                        *owner = g;
                    }
                }
            }
            g
        };
        let open: FxHashSet<FileId> = self.open.keys().copied().collect();
        let limit = self.settings.max_problems.unwrap_or(1000);
        let hooks = self.hooks.clone();
        if let Some(h) = &hooks.problem_check {
            h(true);
        }
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
                let path = a.vfs.path(f);
                let uri = url_of(&path);
                // A newer job owns this file (an edit came in while this one ran): its problems
                // are not computed here at all. What it shows counts against the limit, as its
                // problems would.
                let owned = {
                    let st = published.lock().unwrap();
                    let owned = st.latest.get(&uri) == Some(&generation);
                    if !owned && !open.contains(&f) {
                        total += st.published.get(&uri).copied().unwrap_or(0);
                    }
                    owned
                };
                if let Some(h) = &hooks.before_problems {
                    h(generation, &path, owned);
                }
                if !owned {
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
                // Check and send under the lock, so a newer job cannot publish in between.
                let mut st = published.lock().unwrap();
                if st.latest.get(&uri) != Some(&generation) {
                    continue; // a newer job owns this file
                }
                now.insert(uri.clone());
                if lsp.is_empty() && !st.published.contains_key(&uri) {
                    continue;
                }
                if lsp.is_empty() {
                    st.published.remove(&uri);
                } else {
                    st.published.insert(uri.clone(), lsp.len());
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
                    .keys()
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
            if let Some(h) = &hooks.problem_check {
                h(false);
            }
        });
    }

    // ------------------------------------------------------------------ notifications

    fn on_notification(&mut self, n: Notification) {
        match n.method.as_str() {
            "$/cancelRequest" => {
                let Ok(p) = serde_json::from_value::<CancelParams>(n.params) else { return };
                let id = match p.id {
                    NumberOrString::Number(n) => RequestId::from(n),
                    NumberOrString::String(s) => RequestId::from(s),
                };
                if self.deferred.iter().any(|d| d.id == id) {
                    self.drop_deferred(&[id], None);
                } else if let Some(c) = self.running.lock().unwrap().get(&id) {
                    // Answered by its worker thread.
                    c.store(true, Ordering::Relaxed);
                }
            }
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
                self.token_results.lock().unwrap().open.insert(id);
                if self.analysis.units_of_file(id).is_empty() {
                    self.retry_includes_of(id, None);
                }
                self.note_change(id);
                if self.update.is_some() || self.built.is_some() || self.building {
                    // What that update or build looks for, it may not find in this file: the
                    // same once it is in.
                    self.opened_while_updating.insert(id, (self.edit_seq, false));
                }
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
                // The file on disk is now what the editor has: the text read from disk before
                // is out of date (it is what the file goes back to when closed).
                self.vfs.invalidate(&path);
                if self.building && !is_log(&path) {
                    self.changed_while_building.insert(path.clone());
                }
                // Macro changes reach later files through incremental updates; only a changed
                // build configuration needs a full build.
                if self.is_build_input(&path) {
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
                    let mut tr = self.token_results.lock().unwrap();
                    tr.by_file.remove(&id);
                    tr.open.remove(&id);
                    drop(tr);
                    // Back to the disk contents, also in the units of other open files that
                    // include it.
                    self.note_change(id);
                    if self.analysis.is_adhoc_root(id) {
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
                        // The update thread drops its unit (not open any more); other files may
                        // have used its declarations or macros, which it then reports.
                        self.flush_pending();
                    }
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
                    if self.building && !is_log(&path) {
                        self.changed_while_building.insert(path.clone());
                    }
                    if self.is_build_input(&path) {
                        // Its new text, for the build and for the ranges of its problems.
                        self.vfs.invalidate_all(&path);
                        self.rebuild_at = Some(Instant::now() + Duration::from_millis(500));
                        continue;
                    }
                    let known = self.vfs.lookup(&path).is_some();
                    let ids = self.vfs.invalidate_all(&path);
                    // A new or deleted source changes what auto-configuration finds, which files
                    // a build-file wildcard matches, and whether an `include resolves; so does a
                    // directory holding files of the build.
                    let directory = !known && ids.iter().any(|id| !self.analysis.units_of_file(*id).is_empty());
                    if c.typ != FileChangeType::CHANGED && (auto || is_sv_file(&path) || directory) {
                        self.rebuild_at = Some(Instant::now() + Duration::from_millis(1500));
                    }
                    for id in ids {
                        if !self.vfs.has_overlay(id) {
                            if self.building {
                                // The running build may have read the old text, even of a file
                                // the previous snapshot did not know: replay it afterwards.
                                self.changed_during_build.insert(id);
                            }
                            if !self.analysis.units_of_file(id).is_empty() {
                                self.note_change(id);
                            } else if self.update.is_some() || self.built.is_some() {
                                // Numbered now: a request sent after it waits for it.
                                self.number_edit(id);
                                self.changed_while_updating.insert(id);
                            }
                        }
                    }
                }
            }
            "workspace/didChangeConfiguration" => {
                let Ok(p) = serde_json::from_value::<DidChangeConfigurationParams>(n.params) else { return };
                let v = p.settings.get("dvicode").cloned().unwrap_or(p.settings);
                let kinds = hint_kinds_of(&v);
                let hints_changed = kinds != self.hint_kinds;
                self.hint_kinds = kinds;
                self.format_settings = v.get("format").cloned().unwrap_or(Value::Null);
                let mut rebuild = false;
                if let Ok(s) = serde_json::from_value::<Settings>(v) {
                    // The client sends its configuration once at start-up too.
                    if s != self.settings {
                        log::info!("settings changed: {s:?}");
                        self.settings = s;
                        self.start_build();
                        rebuild = true;
                    }
                }
                // With a build coming, the hints are asked for again once it is in.
                if hints_changed && !rebuild {
                    self.refresh_hints(true);
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
        self.watch_build_inputs();
        // An edit elsewhere (a function's arguments renamed) changes other files' hints.
        self.refresh_hints(false);
    }

    /// Asks the client to request inlay hints again (at most once a second unless `now`).
    fn refresh_hints(&mut self, now: bool) {
        if !self.hint_refresh {
            return;
        }
        if let Some(t) = self.hints_refreshed.filter(|t| !now && t.elapsed() < Duration::from_secs(1)) {
            // Held back, not dropped: the last change's hints are asked for when it allows.
            self.hints_due = Some(t + Duration::from_secs(1));
            return;
        }
        self.hints_due = None;
        self.hints_refreshed = Some(Instant::now());
        self.request_client("workspace/inlayHint/refresh", Value::Null);
    }

    /// Has the client watch the files the build read, whatever their names: the extension's
    /// own watcher covers sources, `.build` and `.f` files, not `-f sources.list` or a compile
    /// log. Registered again when they change.
    fn watch_build_inputs(&mut self) {
        // Not the server's own log, which the build would then rewrite at every build.
        let log = crate::logger::file();
        let inputs: Vec<PathBuf> = self.analysis.setup.config.inputs.iter().filter(|p| !log.is_some_and(|l| build_docs::same_file(p, l))).cloned().collect();
        if !self.watch_inputs || inputs == self.watched {
            return;
        }
        const ID: &str = "dvicode-build-inputs";
        if !self.watched.is_empty() {
            self.request_client("client/unregisterCapability", json!({ "unregisterations": [{ "id": ID, "method": "workspace/didChangeWatchedFiles" }] }));
        }
        // A name is a pattern: its glob characters each stand for one character (`?`), which
        // matches the name itself (and at most a few look-alikes, which are not inputs).
        let literal = |s: &str| s.chars().map(|c| if "*?[]{}\\".contains(c) { '?' } else { c }).collect::<String>();
        let watchers: Vec<Value> = inputs
            .iter()
            // The client's watcher would look at a network host that is not allowed.
            .filter(|p| dvi_project::fsx::blocked(p).is_none())
            .filter_map(|p| {
                let (dir, name) = (p.parent()?, p.file_name()?.to_str()?);
                Some(if self.relative_patterns {
                    json!({ "globPattern": { "baseUri": url_of(dir), "pattern": literal(name) } })
                } else {
                    json!({ "globPattern": format!("{}/{}", absolute_pattern(p)?, literal(name)) })
                })
            })
            .collect();
        if !watchers.is_empty() {
            let id = self.request_client("client/registerCapability", json!({ "registrations": [{ "id": ID, "method": "workspace/didChangeWatchedFiles", "registerOptions": { "watchers": watchers } }] }));
            self.watch_request = Some(id);
        }
        self.watched = inputs;
    }

    /// A file whose change means building again: a build or argument file by its name, or any
    /// file the current build read.
    fn is_build_input(&self, path: &std::path::Path) -> bool {
        if is_log(path) {
            // Written at every build: a log named `server.f`, or one the build also reads.
            return false;
        }
        path.extension().is_some_and(|e| e == "build" || e == "f" || e == "F") || self.analysis.setup.config.inputs.iter().any(|p| same_input(p, path))
    }

    /// Answers `req` with `f` on a worker thread. A request about a file waits for the edits
    /// made before it, of that file and of the files compiled with it, so that it sees the text
    /// the editor sent before it; one about no file waits for all of them. Other edits may still
    /// be analysed meanwhile: what other units declare can be a little older.
    fn spawn<P, R>(&mut self, req: Request, f: impl FnOnce(&Analysis, P) -> Result<R, String> + Send + 'static)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
    {
        self.spawn_with(req, Some(false), f);
    }

    /// [`Server::spawn`] for requests that look through the whole project (references, rename,
    /// subclasses, the views): they wait for every edit made before them.
    fn spawn_project<P, R>(&mut self, req: Request, f: impl FnOnce(&Analysis, P) -> Result<R, String> + Send + 'static)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
    {
        self.spawn_with(req, Some(true), f);
    }

    /// [`Server::spawn`] for requests that do not depend on edits: answered at once.
    fn spawn_now<P, R>(&mut self, req: Request, f: impl FnOnce(&Analysis, P) -> Result<R, String> + Send + 'static)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
    {
        self.spawn_with(req, None, f);
    }

    /// `project`: whether it waits for all earlier edits or for those of its file; `None`: for
    /// none.
    fn spawn_with<P, R>(&mut self, req: Request, project: Option<bool>, f: impl FnOnce(&Analysis, P) -> Result<R, String> + Send + 'static)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
    {
        let id = req.id.clone();
        let file = self.file_of_request(&req.params);
        // Call and type hierarchy items carry their document and position.
        let positional = ["position", "range", "item"].iter().any(|k| req.params.get(k).is_some());
        let params: P = match serde_json::from_value(req.params) {
            Ok(p) => p,
            Err(e) => {
                self.send(Message::Response(Response::new_err(id, ErrorCode::InvalidParams as i32, e.to_string())));
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.running.lock().unwrap().insert(id.clone(), cancel.clone());
        let (sender, running, hooks) = (self.sender.clone(), self.running.clone(), self.hooks.clone());
        let method = req.method.clone();
        let rid = id.clone();
        let plain = !self.document_changes && matches!(method.as_str(), "textDocument/rename" | "textDocument/codeAction");
        let run = move |a: Arc<Analysis>| {
            rayon::spawn(move || {
                let t0 = Instant::now();
                // Not started when it was cancelled while it waited for a thread; its result is
                // thrown away when it was cancelled while it ran (long searches stop early).
                let resp = if cancel.load(Ordering::Relaxed) {
                    cancelled(rid)
                } else {
                    if let Some(h) = &hooks.before_request {
                        h(&method);
                    }
                    let mut a = (*a).clone();
                    a.cancel = Some(cancel.clone());
                    let mut resp = match crash::catch(&method, Some(&sender), || f(&a, params)) {
                        Some(Ok(v)) => ok_response(rid, into_value(v)),
                        Some(Err(msg)) => Response::new_err(rid, ErrorCode::RequestFailed as i32, msg),
                        None => Response::new_err(rid, ErrorCode::InternalError as i32, format!("{method} failed")),
                    };
                    // A client without `documentChanges`: the same edits as `changes`.
                    if plain {
                        if let Some(r) = resp.result.as_mut() {
                            plain_edits(r);
                        }
                    }
                    if cancel.load(Ordering::Relaxed) {
                        cancelled(resp.id)
                    } else {
                        resp
                    }
                };
                let ms = t0.elapsed().as_millis();
                if ms > 200 {
                    log::info!("{method} took {ms} ms");
                }
                running.lock().unwrap().remove(&resp.id);
                let _ = sender.send(Message::Response(resp));
            })
        };
        let mut d = Deferred { id, seq: self.edit_seq, project: project.unwrap_or(false), file, needed: Vec::new(), positional, run: Box::new(run) };
        d.needed = self.dirty.keys().copied().filter(|f| self.needs(&d, *f)).collect();
        if project.is_some() && !d.needed.is_empty() {
            self.deferred.push(d);
            // Its edits are analysed now, not after the pause in typing.
            self.flush_pending();
        } else {
            (d.run)(self.analysis.clone());
        }
    }

    /// Answers waiting requests that will not be answered: they were cancelled (`ids`), or
    /// they name a position in `file`, which changed since they were sent.
    fn drop_deferred(&mut self, ids: &[RequestId], file: Option<FileId>) {
        let mut kept = Vec::new();
        for d in std::mem::take(&mut self.deferred) {
            let resp = if ids.contains(&d.id) {
                cancelled(d.id)
            } else if d.positional && file.is_some() && d.file == file {
                Response::new_err(d.id, ErrorCode::ContentModified as i32, "the document changed".into())
            } else {
                kept.push(d);
                continue;
            };
            self.running.lock().unwrap().remove(&resp.id);
            self.send(Message::Response(resp));
        }
        self.deferred = kept;
    }

    /// The document a request is about (`textDocument.uri`, or the `uri` of a call or type
    /// hierarchy item), when the server knows it.
    /// For completion and signature help on a file with edits the snapshot in use does not have
    /// yet: when they are all on the cursor's line, inside a subroutine or block, and that line
    /// opens or closes nothing (`dvi_analysis::ide::completion::line_local_edit`), the answer
    /// comes at once from that snapshot with the editor's line, rather than after the update
    /// (which for a unit of megabytes takes longer than the pause between two keys). Edits of
    /// other files compiled with it, or a build under way, make it wait as usual.
    fn line_local(&self, req: &Request) -> Option<LineLocal> {
        let file = self.file_of_request(&req.params)?;
        // A build under way, or done and waiting for the edits to be replayed on it, may
        // replace what the snapshot in use says.
        // Edits of any other file may change what this one is compiled with (the macros an
        // earlier file leaves behind), as may a build.
        if self.building || self.built.is_some() || !self.dirty.contains_key(&file) || self.dirty.len() > 1 {
            return None;
        }
        let p: Position = serde_json::from_value(req.params.get("position")?.clone()).ok()?;
        let old = self.analysis.file_text(file)?;
        let new = self.vfs.text(file)?;
        let (line_start, lo, ln) = dvi_analysis::ide::completion::line_local_edit(&old, &new, p.line)?;
        // The cursor's column (UTF-16) in the editor's line, and the same place, or the end, in
        // the snapshot's line.
        let mut col = ln.len();
        let mut units = 0u32;
        for (i, c) in ln.char_indices() {
            if units >= p.character {
                col = i;
                break;
            }
            units += c.len_utf16() as u32;
        }
        let off = self.analysis.line_scope(file, line_start, line_start + lo.len() as u32)?;
        log::debug!("{}: answered from revision {} with the editor's line {}", req.method, self.analysis.revision, p.line + 1);
        Some(LineLocal { file, off, line: ln[..col].to_string(), text: new })
    }

    fn file_of_request(&self, params: &Value) -> Option<FileId> {
        let uri = params.get("textDocument").or_else(|| params.get("item")).and_then(|t| t.get("uri")).and_then(|u| u.as_str());
        uri.and_then(|u| Url::parse(u).ok()).and_then(|u| self.analysis.file_of_url(&u))
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
            self.send(Message::Response(ok_response(req.id.clone(), result)));
            return true;
        }
        // Edited since the last check: check this text now, so that ranges match it.
        let env = project_env(&self.settings);
        let job = self.inspect_job(&key, &env, 0);
        let (id, method, params, sender) = (req.id.clone(), req.method.clone(), req.params.clone(), self.sender.clone());
        rayon::spawn(move || {
            let result = job.and_then(|j| j()).map_or(Value::Null, |ins| build_docs::answer(&method, &params, &ins));
            let _ = sender.send(Message::Response(ok_response(id, result)));
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
            "textDocument/completion" => match self.line_local(&req) {
                Some(l) => self.spawn_now(req, move |a, _: CompletionParams| {
                    if !l.current(a) {
                        return Ok(None);
                    }
                    let items = a.completions_with_line(l.file, l.off, &l.line);
                    Ok(Some(CompletionResponse::List(CompletionList { is_incomplete: items.len() >= 3000, items })))
                }),
                None => self.spawn(req, |a, p: CompletionParams| {
                    let tp = p.text_document_position;
                    let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                    let items = a.completions(f, off);
                    Ok(Some(CompletionResponse::List(CompletionList { is_incomplete: items.len() >= 3000, items })))
                }),
            },
            "textDocument/signatureHelp" => match self.line_local(&req) {
                Some(l) => self.spawn_now(req, move |a, _: SignatureHelpParams| Ok(if l.current(a) { a.signature_help_with_line(l.file, l.off, &l.line) } else { None })),
                None => self.spawn(req, |a, p: SignatureHelpParams| {
                    let tp = p.text_document_position_params;
                    let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                    Ok(a.signature_help(f, off))
                }),
            },
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
            "textDocument/implementation" => self.spawn_project(req, |a, p: GotoDefinitionParams| {
                let tp = p.text_document_position_params;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                Ok(Some(GotoDefinitionResponse::Array(a.goto_implementation(f, off))))
            }),
            "textDocument/references" => self.spawn_project(req, |a, p: ReferenceParams| {
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
            "workspace/symbol" => self.spawn_project(req, |a, p: WorkspaceSymbolParams| Ok(Some(WorkspaceSymbolResponse::Flat(a.workspace_symbols(&p.query, 500))))),
            "textDocument/prepareRename" => self.spawn(req, |a, p: TextDocumentPositionParams| {
                let Some((f, off)) = pos(a, &p.text_document.uri, p.position) else { return Ok(None) };
                Ok(a.prepare_rename(f, off).and_then(|s| a.to_range(s)).map(PrepareRenameResponse::Range))
            }),
            "textDocument/rename" => self.spawn_project(req, |a, p: RenameParams| {
                let tp = p.text_document_position;
                let Some((f, off)) = pos(a, &tp.text_document.uri, tp.position) else { return Ok(None) };
                a.rename(f, off, &p.new_name).map(Some)
            }),
            // Formatting: of the document's text as it is now (unsaved edits too, a file outside
            // the build too), off the main loop.
            "textDocument/formatting" | "textDocument/rangeFormatting" => {
                let range = req.method == "textDocument/rangeFormatting";
                let uri = req.params.get("textDocument").and_then(|t| t.get("uri")).and_then(|u| serde_json::from_value::<Url>(u.clone()).ok());
                let path = uri.as_ref().and_then(path_of);
                let text = path.as_ref().and_then(|p| self.vfs.lookup(p)).and_then(|id| self.vfs.text(id));
                let editor: FormattingOptions = req.params.get("options").cloned().and_then(|o| serde_json::from_value(o).ok()).unwrap_or(FormattingOptions { tab_size: 2, insert_spaces: true, ..Default::default() });
                let lines = range.then(|| req.params.get("range").cloned().and_then(|r| serde_json::from_value::<Range>(r).ok())).flatten().map(|r| dvi_syntax::format::range_lines(r.start.line as usize, r.end.line as usize, r.end.character as usize));
                let settings = self.format_settings.clone();
                let (id, sender) = (req.id.clone(), self.sender.clone());
                rayon::spawn(move || {
                    // A panic is reported and answered with no edit, never left to the pool.
                    let result = crash::catch("formatting", Some(&sender), || match (text, format::options(&settings, path.as_deref(), &editor)) {
                        (Some(text), Some(o)) => format::edits(&text, &o, lines).map_or(Value::Null, |e| json!(e)),
                        _ => Value::Null,
                    })
                    .unwrap_or(Value::Null);
                    let _ = sender.send(Message::Response(ok_response(id, result)));
                });
            }
            "textDocument/inlayHint" => {
                let kinds = self.hint_kinds;
                self.spawn(req, move |a, p: InlayHintParams| {
                    let Some((f, start)) = pos(a, &p.text_document.uri, p.range.start) else { return Ok(None) };
                    let end = a.offset_at(f, p.range.end).unwrap_or(start);
                    let hints: Vec<InlayHint> = a
                        .inlay_hints(f, start, end, kinds)
                        .into_iter()
                        .filter_map(|h| {
                            let at = a.to_range(dvi_syntax::Span::new(f, h.offset, h.offset))?.start;
                            Some(InlayHint {
                                position: at,
                                label: InlayHintLabel::String(h.label),
                                kind: Some(InlayHintKind::PARAMETER),
                                text_edits: None,
                                tooltip: None,
                                padding_left: Some(false),
                                padding_right: Some(true),
                                data: None,
                            })
                        })
                        .collect();
                    Ok(Some(hints))
                })
            }
            "textDocument/semanticTokens/full" => {
                let results = self.token_results.clone();
                self.spawn(req, move |a, p: SemanticTokensParams| {
                    let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(None) };
                    let ((id, data), _) = TokenResults::get(&results, a, f);
                    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: Some(id), data: data.to_vec() })))
                })
            }
            "textDocument/semanticTokens/full/delta" => {
                let results = self.token_results.clone();
                self.spawn(req, move |a, p: SemanticTokensDeltaParams| {
                    let Some(f) = a.file_of_url(&p.text_document.uri) else { return Ok(None) };
                    let ((id, data), before) = TokenResults::get(&results, a, f);
                    // The changes from what the client has, when that is known.
                    let base = match &before {
                        Some((b, d)) if *b == p.previous_result_id => Some(d.as_slice()),
                        None if id == p.previous_result_id => Some(data.as_slice()),
                        _ => None,
                    };
                    Ok(Some(match base {
                        Some(old) => SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta { result_id: Some(id.clone()), edits: token_edits(old, &data) }),
                        None => SemanticTokensFullDeltaResult::Tokens(SemanticTokens { result_id: Some(id), data: data.to_vec() }),
                    }))
                })
            }
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
                self.spawn_project(req, move |a, p: TypeHierarchySupertypesParams| {
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
            "callHierarchy/incomingCalls" => self.spawn_project(req, |a, p: CallHierarchyIncomingCallsParams| {
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
            "textDocument/documentLink" => self.send(Message::Response(ok_response(req.id, Value::Null))),
            "textDocument/codeAction" => self.spawn(req, |a, p: CodeActionParams| {
                // Kinds form a dotted hierarchy: `refactor` asks for `refactor.rewrite`, not `refactor.re`.
                let wants = |kind: &str| p.context.only.as_ref().is_none_or(|o| o.iter().any(|k| k.as_str().is_empty() || kind == k.as_str() || kind.starts_with(&format!("{}.", k.as_str()))));
                let Some((f, start)) = pos(a, &p.text_document.uri, p.range.start) else { return Ok(None) };
                let end = a.offset_at(f, p.range.end).unwrap_or(start);
                let fixes = if wants("quickfix") { a.quick_fixes(f, start, end) } else { Vec::new() };
                let mut actions: Vec<CodeActionOrCommand> = fixes
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
                if wants("refactor.rewrite") {
                    let refactors = a.implement_pure_methods(f, start).into_iter().chain(a.instance_actions(f, start));
                    for r in refactors {
                        if let Some(edit) = a.versioned_edit(r.edits) {
                            actions.push(CodeActionOrCommand::CodeAction(CodeAction { title: r.title, kind: Some(CodeActionKind::REFACTOR_REWRITE), edit: Some(edit), ..Default::default() }));
                        }
                    }
                }
                Ok(Some(actions))
            }),
            // ---------------------------------------------------------- custom
            "dvicode/status" => self.spawn_project(req, |a, _: Value| Ok(a.status())),
            "dvicode/serverInfo" => self.spawn_now(req, |_, _: Value| {
                Ok(json!({
                    "version": crate::VERSION,
                    "pid": std::process::id(),
                    "host": fs::hostname(),
                    "os": std::env::consts::OS,
                    "arch": std::env::consts::ARCH,
                    "home": std::env::var("HOME").unwrap_or_default(),
                }))
            }),
            "dvicode/compiledFiles" => self.spawn_project(req, |a, _: Value| Ok(a.compiled_files())),
            "dvicode/designTops" => self.spawn_project(req, |a, _: Value| {
                Ok(a.design_tops().into_iter().map(|r| a.decl(r).name.to_string()).collect::<Vec<_>>())
            }),
            "dvicode/designHierarchy" => self.spawn_project(req, |a, p: Value| {
                let top = p.get("top").and_then(|t| t.as_str()).map(|s| s.to_string());
                Ok(a.design_hierarchy(top.as_deref(), 20000))
            }),
            "dvicode/verificationHierarchy" => self.spawn_project(req, |a, p: Value| {
                let test = p.get("test").and_then(|t| t.as_str()).filter(|t| !t.is_empty()).map(|s| s.to_string());
                Ok(a.verification_hierarchy(test.as_deref(), 20000))
            }),
            // The views: one tree, filtered and limited (see TreeParams).
            "dvicode/tree" => {
                let models = self.register_models.clone();
                self.spawn_project(req, move |a, p: TreeParams| Ok(view_tree(a, p, &models)))
            }
            "dvicode/registerFields" => {
                let models = self.register_models.clone();
                self.spawn_project(req, move |a, p: RegisterFieldsParams| {
                    if p.revision.is_some_and(|r| u64::try_from(r).ok() != Some(a.revision)) {
                        return Ok(json!({ "revision": a.revision, "stale": true }));
                    }
                    let Some((_, model)) = register_model(a, p.top.as_deref(), &models) else { return Ok(Value::Null) };
                    let fields: Vec<Value> = a
                        .register_fields(&model, p.register)
                        .into_iter()
                        .map(|f| {
                            let range = f.lsb.zip(f.size).and_then(|(l, s)| if s <= 1 { Some(format!("[{l}]")) } else { l.checked_add(s - 1).map(|h| format!("[{h}:{l}]")) });
                            let at = a.to_range(f.at).map(|r| r.start);
                            json!({
                                "name": f.name, "kind": "field", "range": range, "access": f.access, "reset": f.reset, "volatile": f.volatile, "notes": f.notes,
                                "uri": at.map(|_| dvi_analysis::ide::url_of(&a.path(f.at.file)).to_string()), "line": at.map(|p| p.line), "character": at.map(|p| p.character),
                            })
                        })
                        .collect();
                    Ok(json!({ "revision": a.revision, "fields": fields }))
                })
            }
            "dvicode/registerAt" => {
                let models = self.register_models.clone();
                self.spawn_project(req, move |a, p: RegisterAtParams| {
                    let Some(addr) = parse_address(&p.address) else { return Err(format!("not an address: {}", p.address)) };
                    let Some((top, model)) = register_model(a, p.top.as_deref(), &models) else { return Ok(Value::Null) };
                    Ok(json!({ "revision": a.revision, "top": top, "hits": a.registers_at(&model, addr), "cut": model.cut }))
                })
            }
            // The assertions and coverage of the declaration at a position (null: none there).
            "dvicode/checksOf" => self.spawn_project(req, |a, p: LocateParams| {
                let Some((f, off)) = pos(a, &p.text_document.uri, p.position) else { return Ok(Value::Null) };
                Ok(match a.checks_at(f, off) {
                    Some(r) => json!({ "revision": a.revision, "target": r.target, "hits": r.hits, "limits": r.limits, "notes": r.notes }),
                    None => Value::Null,
                })
            }),
            // The writes or reads of the declaration at a position (null: no signal there).
            "dvicode/accessesOf" => self.spawn_project(req, |a, p: AccessesParams| {
                let Some((f, off)) = pos(a, &p.text_document.uri, p.position) else { return Ok(Value::Null) };
                let writes = match p.mode.as_str() {
                    "writes" => true,
                    "reads" => false,
                    m => return Err(format!("unknown mode {m}")),
                };
                Ok(match a.accesses_at(f, off, writes) {
                    Some(r) => json!({ "revision": a.revision, "target": r.target, "mode": r.mode, "sites": r.sites, "limits": r.limits, "notes": r.notes }),
                    None => Value::Null,
                })
            }),
            "dvicode/locate" => self.spawn_project(req, |a, p: LocateParams| {
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
            "dvicode/compileContext" => self.spawn_project(req, |a, p: TextDocumentIdentifierParams| {
                let Some(path) = path_of(&p.text_document.uri) else { return Err("not a file".to_string()) };
                Ok(a.compile_context(&path, a.file_of_url(&p.text_document.uri)))
            }),
            "dvicode/includingFiles" => self.spawn_project(req, |a, p: TextDocumentIdentifierParams| {
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
                self.spawn_now(req, move |_, _: Value| {
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
                self.send(Message::Response(ok_response(req.id, Value::Null)));
            }
            m if m.starts_with("dvicode/fs/") => {
                let op = m.trim_start_matches("dvicode/fs/").to_string();
                let id = req.id.clone();
                let params = req.params;
                let sender = self.sender.clone();
                rayon::spawn(move || {
                    let feature = format!("remote file access ({op})");
                    let resp = match crash::catch(&feature, Some(&sender), || fs::handle(&op, params)) {
                        Some(Ok(v)) => ok_response(id, v),
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

/// The reply to a cancelled request.
fn cancelled(id: RequestId) -> Response {
    Response::new_err(id, ErrorCode::RequestCanceled as i32, "cancelled".into())
}

/// Applies edits to snapshots, one job after another, away from the loop that reads the
/// editor's messages: compiling a header again that thousands of files include takes long.
fn update_thread(jobs: Receiver<UpdateJob>, events: Sender<Event>, client: Sender<Message>, hooks: Arc<Hooks>) {
    while let Ok(mut job) = jobs.recv() {
        // Only the newest job can be one the loop still waits for: the others were replaced.
        while let Ok(newer) = jobs.try_recv() {
            job = newer;
        }
        if job.progress.is_cancelled() {
            continue;
        }
        if let Some(h) = &hooks.before_update {
            h();
        }
        let t0 = Instant::now();
        let r = crash::catch("update after an edit", Some(&client), || {
            // Files closed while an update that counted them as open ran keep no unit of
            // their own (closing one replaces the update only when it had one already).
            let mut base = (*job.base).clone();
            base.pin = Some(job.pin.clone());
            let closed: Vec<FileId> = base.units.iter().flatten().filter(|u| u.kind == UnitKind::AdHoc && job.open.binary_search(&u.root).is_err()).map(|u| u.root).collect();
            for f in closed {
                base = base.remove_adhoc(f);
            }
            // An open header may have lost its last includer, or an open file outside the
            // build gained one.
            let mut next = base.update_with(&job.files, &job.progress)?.sync_open_files_with(&job.open, &job.progress)?;
            next.pin = None;
            Some(next)
        });
        let done = match r {
            Some(None) => continue, // cancelled: a newer job replaces it
            Some(Some(mut next)) => {
                let interface_changed = next.interface_changed(&job.base);
                let recompiled = next.recompiled_files(&job.base);
                if !interface_changed {
                    // Other files' skipped-code uses cannot have changed: only the edited
                    // files are checked again for them.
                    next.reuse_hidden_uses(&job.base);
                }
                log::debug!("incremental update of {} file(s) in {} ms", job.files.len(), t0.elapsed().as_millis());
                Some(Box::new(Updated { analysis: next, interface_changed, recompiled }))
            }
            None => None,
        };
        if let Some(h) = &hooks.after_update {
            h();
        }
        let _ = events.send(Event::Updated(job.seq, done));
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
    /// Design Hierarchy: generate branches proven not elaborated are shown.
    inactive: Option<bool>,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessesParams {
    text_document: TextDocumentIdentifier,
    position: Position,
    /// `writes` or `reads`.
    mode: String,
}

/// The register model `top` builds in `a` (the default top when `None`), from `models` when
/// it was built for this revision.
fn register_model(a: &Analysis, top: Option<&str>, models: &RegisterModels) -> Option<(String, Arc<dvi_analysis::ide::registers::Model>)> {
    let (chosen, ..) = a.register_top(top.filter(|t| !t.is_empty()));
    let chosen = chosen?;
    let name = a.decl(chosen).name.to_string();
    if let Some((rev, n, m)) = models.lock().unwrap().as_ref() {
        if *rev == a.revision && *n == name {
            return Some((name, m.clone()));
        }
    }
    let m = Arc::new(a.register_model(chosen));
    *models.lock().unwrap() = Some((a.revision, name.clone(), m.clone()));
    Some((name, m))
}

/// An address as typed: `0x10`, `'h10`, `32'h10` or `10` (hexadecimal), `'d16` or `32'd16`
/// (decimal).
fn parse_address(s: &str) -> Option<u128> {
    let s: String = s.trim().chars().filter(|c| *c != '_').collect::<String>().to_ascii_lowercase();
    let (width, digits, radix) = match s.split_once('\'') {
        Some((w, rest)) => match rest.split_at_checked(1)? {
            ("h", h) => (w, h, 16),
            ("d", d) => (w, d, 10),
            _ => return None,
        },
        None => ("", s.strip_prefix("0x").unwrap_or(&s), 16),
    };
    if !width.chars().all(|c| c.is_ascii_digit()) || digits.is_empty() {
        return None;
    }
    u128::from_str_radix(digits, radix).ok()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterFieldsParams {
    top: Option<String>,
    register: usize,
    /// The revision of the tree the register is in (the client's -1: none yet).
    revision: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterAtParams {
    top: Option<String>,
    address: String,
}

/// The last register model built: (revision, top, model).
type RegisterModels = Arc<Mutex<Option<(u64, String, Arc<dvi_analysis::ide::registers::Model>)>>>;

fn view_tree(a: &Analysis, p: TreeParams, models: &RegisterModels) -> Value {
    use dvi_analysis::ide::filter::Filter;
    use dvi_analysis::ide::views::TreeQuery;
    let mut q = TreeQuery::new(p.max_nodes.unwrap_or(20_000).min(100_000));
    q.filter = p.filter.as_deref().and_then(Filter::new);
    q.reveal = p.reveal.unwrap_or_default();
    q.target = p.target;
    q.ports = p.ports.unwrap_or(true);
    q.objects = p.objects.unwrap_or(false);
    q.inactive = p.inactive.unwrap_or(false);
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
        "registers" => {
            let (chosen, blocks, tops) = a.register_top(nonempty(p.top.clone()).as_deref());
            let names = |v: &[dvi_analysis::DeclRef]| v.iter().map(|b| a.decl(*b).name.to_string()).collect::<Vec<_>>();
            let Some((top, model)) = chosen.and_then(|_| register_model(a, nonempty(p.top).as_deref(), models)) else {
                return json!({ "revision": a.revision, "roots": [], "blocks": names(&blocks), "tops": names(&tops) });
            };
            // The whole tree is filtered first, then cut to the node limit (keeping `reveal`).
            let (tree, _) = a.register_tree(&model, 1_000_000);
            let mut matches = 0;
            let mut roots: Vec<dvi_analysis::ide::registers::RegNode> = match &q.filter {
                Some(f) => dvi_analysis::ide::registers::prune(tree, f, &mut matches).into_iter().collect(),
                None => vec![tree],
            };
            let reveal = q.reveal.get(1..).unwrap_or(&[]);
            let truncated = roots.iter_mut().fold(false, |t, r| dvi_analysis::ide::registers::limit_tree(r, q.max_nodes, reveal) | t);
            let incomplete = if truncated { Some("nodes") } else { None };
            json!({ "revision": a.revision, "incomplete": incomplete, "matches": matches, "roots": roots, "top": top, "blocks": names(&blocks), "tops": names(&tops), "cut": model.cut })
        }
        "verification" => {
            let (h, meta) = a.verification_tree(nonempty(p.test).as_deref(), &q);
            json!({ "revision": a.revision, "incomplete": meta.incomplete, "matches": meta.matches, "roots": h.roots, "test": h.test, "configured": h.configured, "tests": h.tests, "hint": h.hint })
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
/// The server's own log file.
fn is_log(p: &std::path::Path) -> bool {
    // By identity, whatever its name (a link to the log).
    crate::logger::file().is_some_and(|l| l == p || dvi_project::fsx::same_file(l, p))
}

/// One build input under two spellings: the same file on disk, or (where case does not matter)
/// the same path ignoring case, also once the file is deleted.
fn same_input(input: &std::path::Path, p: &std::path::Path) -> bool {
    let folded = |x: &std::path::Path| x.to_string_lossy().to_lowercase();
    input == p || build_docs::same_file(input, p) || (dvi_project::fsx::ignores_case(input) && folded(input) == folded(p))
}

/// A folder as an absolute glob pattern for a client without relative patterns: forward
/// slashes on Windows, no `\\?\` prefix, glob characters of its names standing for one
/// character each.
fn absolute_pattern(p: &std::path::Path) -> Option<String> {
    let dir = p.parent()?.to_string_lossy().into_owned();
    let dir = dir.strip_prefix(r"\\?\").unwrap_or(&dir);
    let dir = if cfg!(windows) { dir.replace('\\', "/") } else { dir.to_string() };
    Some(dir.chars().map(|c| if "*?[]{}".contains(c) || (!cfg!(windows) && c == '\\') { '?' } else { c }).collect())
}

fn is_sv_file(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "sv" | "svh" | "svi" | "svp" | "sva" | "v" | "vh" | "vp" | "vlib"))
}

fn pos(a: &Analysis, uri: &Url, p: Position) -> Option<(FileId, u32)> {
    let f = a.file_of_url(uri)?;
    Some((f, a.offset_at(f, p)?))
}

#[cfg(test)]
mod tests;

/// Workspace edits in `v` given as `documentChanges` (text edits only), given as `changes` by
/// document instead, for a client that does not take the former.
fn plain_edits(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(plain_edits),
        Value::Object(o) => {
            let text_only = o.get("documentChanges").and_then(Value::as_array).is_some_and(|d| d.iter().all(|e| e.get("textDocument").is_some() && e.get("edits").is_some()));
            if text_only {
                let mut changes = serde_json::Map::new();
                for e in o.remove("documentChanges").and_then(|d| if let Value::Array(a) = d { Some(a) } else { None }).unwrap_or_default() {
                    let uri = e["textDocument"]["uri"].as_str().unwrap_or_default().to_string();
                    let edits = e["edits"].as_array().cloned().unwrap_or_default();
                    if let Value::Array(list) = changes.entry(uri).or_insert_with(|| Value::Array(Vec::new())) {
                        list.extend(edits);
                    }
                }
                o.insert("changes".into(), Value::Object(changes));
            }
            o.values_mut().for_each(plain_edits);
        }
        _ => {}
    }
}

/// A success answer holding `v` itself (`Response::new_ok` would serialize it into a copy).
fn ok_response(id: RequestId, v: Value) -> Response {
    Response { id, result: Some(v), error: None }
}

/// `r` as JSON: moved when it is JSON already, else serialized.
fn into_value<R: serde::Serialize + 'static>(r: R) -> Value {
    let mut slot = Some(r);
    if let Some(v) = (&mut slot as &mut dyn std::any::Any).downcast_mut::<Option<Value>>() {
        return v.take().unwrap_or(Value::Null);
    }
    slot.map_or(Value::Null, |r| serde_json::to_value(r).unwrap_or(Value::Null))
}
