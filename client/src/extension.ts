// DVICode VS Code extension: starts the language server where it should run and adds the
// DVICode views and commands.

import * as vscode from 'vscode';
import { LanguageClient, LanguageClientOptions, RevealOutputChannelOn, State } from 'vscode-languageclient/node';
import { Launch, makeLaunch, serverSettingsKey } from './launcher';
import { Mapper } from './pathmap';
import { RemoteFs } from './remotefs';
import { InternalError, LogChannel, Reporter } from './report';
import { Structure } from './structure';
import { VerificationHierarchy, Views, componentClassLocation, hierarchyPath } from './views';

interface Status {
  root: string;
  build_file: string | null;
  invocations: number;
  top_files: number;
  files: number;
  units: number;
  declarations: number;
  macros: number;
  packages: number;
  classes: number;
  modules: number;
  build_ms: number;
  problems_in_build_file: number;
  /** Set when the full build failed and the project is empty. */
  build_error?: string;
}

let client: LanguageClient | undefined;
let launch: Launch | undefined;
let starting: Promise<LanguageClient> | undefined;
let restarting: Promise<void> | undefined;
/** serverSettingsKey() of the last start, and of the last restart offered. */
let startedWith: string | undefined;
let offeredFor: string | undefined;
/** File watcher of the current client; the language client does not dispose it. */
let watcher: vscode.FileSystemWatcher | undefined;
let output: LogChannel;
let reporter: Reporter;
let statusItem: vscode.StatusBarItem;
let lastStatus: Status | undefined;
let views: Views;
let structure: Structure;

const SV_SELECTOR = [
  { scheme: 'file', language: 'systemverilog' },
  { scheme: 'file', language: 'verilog' },
  { scheme: 'dvicode', language: 'systemverilog' },
  { scheme: 'dvicode', language: 'verilog' },
  // Build and argument files: checked as they are typed, with hover and links (not compiled).
  { scheme: 'file', language: 'dvicode-build' },
  { scheme: 'dvicode', language: 'dvicode-build' },
];

function settingsForServer(): object {
  const c = vscode.workspace.getConfiguration('dvicode');
  const maxProblems = c.get<number>('maxProblems', 1000);
  return {
    buildFile: c.get<string>('buildFile', '') || null,
    env: c.get<Record<string, string>>('env', {}),
    uvmHome: c.get<string>('uvmHome', '') || null,
    maxProblems,
  };
}

/** Resolves true when the server settings differ from `key`, false after `timeoutMs`. */
function serverSettingsChange(key: string, timeoutMs: number): Promise<boolean> {
  return new Promise((resolve) => {
    const done = (changed: boolean): void => {
      clearTimeout(t);
      sub.dispose();
      resolve(changed);
    };
    const sub = vscode.workspace.onDidChangeConfiguration(() => {
      if (serverSettingsKey() !== key) {
        done(true);
      }
    });
    const t = setTimeout(() => done(serverSettingsKey() !== key), timeoutMs);
    if (serverSettingsKey() !== key) {
      done(true);
    }
  });
}

async function startClient(ctx: vscode.ExtensionContext): Promise<LanguageClient> {
  statusItem.text = '$(sync~spin) DVICode';
  statusItem.tooltip = 'Starting the DVICode language server';
  statusItem.show();
  const first = startedWith === undefined;
  startedWith = serverSettingsKey();
  try {
    launch = await makeLaunch(ctx, output);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    // In a remote window, the local user's values of the server settings (a Windows user's
    // `dvicode.server.mode: wsl`, say) reach this extension host for a moment at startup, until
    // VS Code drops them as machine settings of another machine. A first start that fails is
    // tried again when the settings change soon after.
    if (first && vscode.env.remoteName && (await serverSettingsChange(startedWith, 5000))) {
      output.appendLine(`Cannot start the server (${msg}); the settings changed, starting again`);
      return startClient(ctx);
    }
    statusItem.text = '$(error) DVICode';
    statusItem.tooltip = msg;
    output.appendLine(`Cannot start the server: ${msg}`);
    void vscode.window.showErrorMessage(`DVICode: ${msg}`, 'Open Settings').then((a) => {
      if (a) {
        void vscode.commands.executeCommand('workbench.action.openSettings', 'dvicode.server');
      }
    });
    throw e;
  }
  const mapper = launch.mapper;
  output.appendLine(`Server: ${launch.target} (mode ${launch.mode}); path mapping: ${mapper.describe()}`);
  watcher?.dispose();
  watcher = vscode.workspace.createFileSystemWatcher('**/*.{sv,svh,svi,svp,sva,v,vh,vp,vlib,build,f,F}');
  const clientOptions: LanguageClientOptions = {
    documentSelector: SV_SELECTOR,
    outputChannel: output,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
    errorHandler: reporter.errorHandler(),
    initializationOptions: settingsForServer(),
    synchronize: {
      configurationSection: 'dvicode',
      fileEvents: watcher,
    },
    uriConverters: {
      code2Protocol: (uri) => mapper.toServer(uri),
      protocol2Code: (s) => mapper.toClient(s),
    },
  };
  const c = new LanguageClient('dvicode', 'DVICode', launch.serverOptions, clientOptions);
  c.onDidChangeState((e) => {
    if (e.newState === State.Stopped) {
      statusItem.text = '$(debug-disconnect) DVICode';
      statusItem.tooltip = 'The DVICode language server stopped. Click to restart.';
      statusItem.command = 'dvicode.restartServer';
      views.presentAll();
      structure.reset();
    }
  });
  c.onNotification('dvicode/internalError', (e: InternalError) => reporter.onInternalError(e));
  await c.start();
  // What this engine's views support (an engine at a custom path or remote may be older).
  const experimental = c.initializeResult?.capabilities.experimental as { dvicode?: { verificationObjects?: number; structure?: number } } | undefined;
  void vscode.commands.executeCommand('setContext', 'dvicode.engine.objects', (experimental?.dvicode?.verificationObjects ?? 0) >= 1);
  void vscode.commands.executeCommand('setContext', 'dvicode.engine.structure', (experimental?.dvicode?.structure ?? 0) >= 1);
  c.onNotification('dvicode/status', (s: Status) => {
    lastStatus = s;
    if (s.build_error) {
      statusItem.text = '$(warning) DVICode';
      statusItem.tooltip = `${s.build_error}, so DVICode knows nothing about the project. Click to report it.`;
      statusItem.command = 'dvicode.reportIssue';
    } else {
      statusItem.text = `$(circuit-board) DVICode`;
      statusItem.tooltip = new vscode.MarkdownString(
        `**DVICode** on ${launch?.target}\n\n${s.top_files} top files, ${s.files} files, ${s.declarations} declarations, ` +
          `${s.packages} packages, ${s.classes} classes, ${s.modules} design elements\n\n` +
          `Build: ${s.build_file ?? 'auto-configuration'} (${s.build_ms} ms)`,
      );
      statusItem.command = 'dvicode.showStatus';
    }
    views.refreshAll();
    structure.reset();
  });
  c.onNotification('dvicode/revision', (r: { revision?: number }) => {
    views.onRevision(r?.revision);
    structure.onRevision();
  });
  client = c;
  statusItem.text = '$(sync~spin) DVICode';
  statusItem.tooltip = `Building the project on ${launch.target}`;
  return c;
}

/** The running client, starting it if needed. */
function ensureClient(ctx: vscode.ExtensionContext): Promise<LanguageClient> {
  if (client) {
    return Promise.resolve(client);
  }
  if (!starting) {
    starting = startClient(ctx).finally(() => {
      starting = undefined;
    });
  }
  return starting;
}

/**
 * Stops a client. The language client cannot stop while it is starting (after a crash it
 * restarts itself), so a start in progress is waited for first.
 */
async function stopClient(c: LanguageClient): Promise<void> {
  if (c.state === State.Starting) {
    await new Promise<void>((resolve) => {
      const done = (): void => {
        clearTimeout(t);
        sub.dispose();
        resolve();
      };
      const sub = c.onDidChangeState((e) => {
        if (e.newState !== State.Starting) {
          done();
        }
      });
      const t = setTimeout(done, 15000);
    });
  }
  try {
    await c.stop(5000);
  } catch {
    // already gone
  }
}

function restart(ctx: vscode.ExtensionContext): Promise<void> {
  // One restart at a time; a second request while one runs joins it.
  restarting ??= (async () => {
    await starting?.catch(() => undefined);
    const c = client;
    client = undefined;
    if (c) {
      await stopClient(c);
    }
    await ensureClient(ctx);
  })().finally(() => {
    restarting = undefined;
  });
  return restarting;
}

/**
 * Offers a restart when the server settings differ from those the server started with. They are
 * compared rather than asked for with affectsConfiguration: the change event with which VS Code
 * drops another machine's settings in a remote window names no section.
 */
function offerRestart(ctx: vscode.ExtensionContext): void {
  const busy = restarting ?? starting;
  if (busy) {
    // Checked again with the settings that start used.
    void busy.then(
      () => offerRestart(ctx),
      () => offerRestart(ctx),
    );
    return;
  }
  const key = serverSettingsKey();
  if (startedWith === undefined || key === startedWith || key === offeredFor) {
    return;
  }
  offeredFor = key;
  void vscode.window.showInformationMessage('DVICode: server settings changed.', 'Restart Server').then((a) => {
    if (a) {
      void restart(ctx);
    }
  });
}

function editorPosition(): { uri: string; position: vscode.Position } | undefined {
  const e = vscode.window.activeTextEditor;
  if (!e || !launch) {
    return undefined;
  }
  return { uri: launch.mapper.toServer(e.document.uri), position: e.selection.active };
}

async function expandMacro(all: boolean): Promise<void> {
  const p = editorPosition();
  if (!p || !client) {
    return;
  }
  const r = await client.sendRequest<{ text: string } | null>('dvicode/expandMacro', {
    textDocument: { uri: p.uri },
    position: p.position,
    all,
  });
  if (!r) {
    void vscode.window.showInformationMessage('DVICode: place the cursor on a macro call to expand it.');
    return;
  }
  const doc = await vscode.workspace.openTextDocument({ language: 'systemverilog', content: r.text });
  await vscode.window.showTextDocument(doc, { viewColumn: vscode.ViewColumn.Beside, preview: true });
}

async function showIncludingFiles(): Promise<void> {
  const p = editorPosition();
  if (!p || !client || !launch) {
    return;
  }
  const locs = await client.sendRequest<{ uri: string; range: { start: { line: number; character: number } } }[]>('dvicode/includingFiles', {
    textDocument: { uri: p.uri },
  });
  if (!locs.length) {
    void vscode.window.showInformationMessage('DVICode: no file includes this file.');
    return;
  }
  const items = locs.map((l) => {
    const uri = launch!.mapper.toClient(l.uri);
    return { label: vscode.workspace.asRelativePath(uri), description: `line ${l.range.start.line + 1}`, uri, line: l.range.start.line };
  });
  const pick = items.length === 1 ? items[0] : await vscode.window.showQuickPick(items, { placeHolder: 'Files that include this file' });
  if (pick) {
    const pos = new vscode.Position(pick.line, 0);
    await vscode.window.showTextDocument(pick.uri, { selection: new vscode.Range(pos, pos) });
  }
}

async function selectBuildConfig(): Promise<void> {
  if (!client || !launch) {
    return;
  }
  const r = await client.sendRequest<{ current: string | null; available: string[] }>('dvicode/buildConfigs');
  const items: (vscode.QuickPickItem & { value: string })[] = [
    { label: 'Default', description: '.dvicode/default.build if it exists, else auto-configuration', value: '' },
    { label: 'Auto-configuration', description: 'ignore build files and find the sources', value: 'auto' },
    ...r.available.map((p) => {
      const local = launch!.mapper.toClient(vscode.Uri.from({ scheme: 'file', path: p }).toString());
      return { label: p.split('/').slice(-2).join('/'), description: p === r.current ? 'current' : undefined, detail: local.fsPath, value: p };
    }),
  ];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: 'Build configuration' });
  if (pick) {
    await vscode.workspace.getConfiguration('dvicode').update('buildFile', pick.value, vscode.ConfigurationTarget.Workspace);
  }
}

async function selectDesignTop(): Promise<void> {
  if (!client) {
    return;
  }
  const tops = await client.sendRequest<string[]>('dvicode/designTops');
  const items = [{ label: '$(sparkle) Automatic', value: undefined as string | undefined }, ...tops.map((t) => ({ label: t, value: t as string | undefined }))];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: 'Top of the design hierarchy (any module name works)' });
  if (pick) {
    views.design.top = pick.value;
    void views.design.refresh().then(() => views.followAgain(views.design as never));
  }
}

async function selectUvmTest(): Promise<void> {
  if (!client) {
    return;
  }
  const r = await client.sendRequest<{ configured: string | null; tests: string[] }>('dvicode/verificationHierarchy', { test: '*' });
  const items: (vscode.QuickPickItem & { value: string | undefined })[] = [
    { label: '$(settings) From the build configuration', description: r.configured ? `+UVM_TESTNAME=${r.configured}` : 'not set: all tests', value: undefined },
    { label: '$(list-tree) All tests', value: '*' },
    ...r.tests.map((t) => ({ label: t, description: t === r.configured ? '+UVM_TESTNAME' : undefined, value: t as string | undefined })),
  ];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: 'UVM test of the verification hierarchy' });
  if (pick) {
    views.verification.test = pick.value;
    void views.verification.refresh().then(() => views.followAgain(views.verification as never));
  }
}

async function showStatus(): Promise<void> {
  if (!client) {
    return;
  }
  const s = await client.sendRequest<Status>('dvicode/status');
  const info = await client.sendRequest<{ version: string; host: string; os: string; arch: string; pid: number }>('dvicode/serverInfo');
  const lines = [
    `Server ${info.version} on ${info.host} (${info.os}/${info.arch}, pid ${info.pid}) via ${launch?.target}`,
    `Project ${s.root}`,
    `Build ${s.build_file ?? 'auto-configuration'}: ${s.invocations} invocation(s), ${s.top_files} top files, ${s.files} files`,
    `${s.declarations} declarations, ${s.macros} macros, ${s.packages} packages, ${s.classes} classes, ${s.modules} design elements`,
    `Full build ${s.build_ms} ms; path mapping: ${launch?.mapper.describe()}`,
  ];
  const pick = await vscode.window.showQuickPick(
    [
      ...lines.map((l) => ({ label: l })),
      { label: '$(output) Show server output' },
      { label: '$(refresh) Rebuild project' },
      { label: '$(report) Report an issue' },
    ],
    { placeHolder: 'DVICode status' },
  );
  if (pick?.label.includes('Show server output')) {
    output.show();
  } else if (pick?.label.includes('Rebuild')) {
    await client.sendRequest('dvicode/rebuild');
  } else if (pick?.label.includes('Report an issue')) {
    await reporter.reportIssue();
  }
}

async function openRemoteFolder(): Promise<void> {
  const cfg = vscode.workspace.getConfiguration('dvicode');
  const choices: { label: string; authority: string }[] = [];
  if (process.platform === 'win32') {
    choices.push({ label: '$(terminal-linux) WSL (default distribution)', authority: 'wsl+default' });
    const d = cfg.get<string>('server.wslDistro', '');
    if (d) {
      choices.push({ label: `$(terminal-linux) WSL ${d}`, authority: `wsl+${encodeURIComponent(d)}` });
    }
  }
  const host = cfg.get<string>('server.sshHost', '');
  if (host) {
    choices.push({ label: `$(remote) SSH ${host}`, authority: `ssh+${encodeURIComponent(host)}` });
  }
  choices.push({ label: '$(remote) SSH host...', authority: 'ssh+?' });
  const pick = await vscode.window.showQuickPick(choices, { placeHolder: 'Where is the folder?' });
  if (!pick) {
    return;
  }
  let authority = pick.authority;
  if (authority === 'ssh+?') {
    const h = await vscode.window.showInputBox({ prompt: 'SSH destination (user@host or a Host alias)', placeHolder: 'me@build-server' });
    if (!h) {
      return;
    }
    authority = `ssh+${encodeURIComponent(h)}`;
  }
  if (authority === 'wsl+default') {
    const { execFileSync } = await import('child_process');
    const name = execFileSync('wsl.exe', ['-e', 'sh', '-c', 'echo $WSL_DISTRO_NAME']).toString().replace(/\u0000/g, '').trim();
    authority = `wsl+${encodeURIComponent(name)}`;
  }
  const folder = await vscode.window.showInputBox({ prompt: 'Absolute folder path on that machine', placeHolder: '/home/me/project', value: '/home/' });
  if (!folder || !folder.startsWith('/')) {
    return;
  }
  const uri = vscode.Uri.from({ scheme: 'dvicode', authority, path: folder });
  await vscode.commands.executeCommand('vscode.openFolder', uri, { forceNewWindow: false });
}

/** Returned from activate() for tests and other extensions. */
export interface DvicodeApi {
  ready(): Promise<LanguageClient>;
  target(): string | undefined;
  toServer(uri: vscode.Uri): string | undefined;
  verification(): VerificationHierarchy;
  /** The three views (tests). */
  views(): Views;
  /** The Structure view (tests). */
  structure(): Structure;
  /** The last internal error the engine reported. */
  lastInternalError(): InternalError | undefined;
  /** The text DVICode: Report Issue fills in. */
  issueBody(): string;
}

export async function activate(ctx: vscode.ExtensionContext): Promise<DvicodeApi> {
  output = new LogChannel(vscode.window.createOutputChannel('DVICode'));
  reporter = new Reporter(
    output,
    () => ({
      extensionVersion: String(ctx.extension.packageJSON.version),
      engineVersion: client?.initializeResult?.serverInfo?.version,
      target: launch?.target,
      mode: launch?.mode,
      buildFile: lastStatus?.build_file,
      files: lastStatus?.files,
    }),
    ctx.extension.id,
  );
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
  ctx.subscriptions.push(output, statusItem);

  const conn = {
    client: () => client,
    mapper: (): Mapper | undefined => launch?.mapper,
  };
  views = new Views(ctx, conn);
  structure = new Structure(ctx, conn);
  ctx.subscriptions.push(
    views,
    structure,
    vscode.commands.registerCommand('dvicode.selectUvmTest', selectUvmTest),
    vscode.commands.registerCommand('dvicode.openComponentClass', async (item: unknown) => {
      const l = componentClassLocation(item, conn);
      if (l) {
        const pos = new vscode.Position(l.line, 0);
        await vscode.window.showTextDocument(l.uri, { selection: new vscode.Range(pos, pos) });
      }
    }),
    vscode.workspace.registerFileSystemProvider(
      'dvicode',
      new RemoteFs(
        () => ensureClient(ctx),
        () => launch?.authority,
      ),
      { isCaseSensitive: true },
    ),
    vscode.commands.registerCommand('dvicode.restartServer', () => restart(ctx)),
    vscode.commands.registerCommand('dvicode.rebuild', () => client?.sendRequest('dvicode/rebuild')),
    vscode.commands.registerCommand('dvicode.showStatus', showStatus),
    vscode.commands.registerCommand('dvicode.selectBuildConfig', selectBuildConfig),
    vscode.commands.registerCommand('dvicode.expandMacro', () => expandMacro(false)),
    vscode.commands.registerCommand('dvicode.expandMacroAll', () => expandMacro(true)),
    vscode.commands.registerCommand('dvicode.showIncludingFiles', showIncludingFiles),
    vscode.commands.registerCommand('dvicode.openRemoteFolder', openRemoteFolder),
    vscode.commands.registerCommand('dvicode.selectDesignTop', selectDesignTop),
    vscode.commands.registerCommand('dvicode.refreshViews', () => {
      views.refreshAll();
      structure.reset();
    }),
    vscode.commands.registerCommand('dvicode.showOutput', () => output.show()),
    vscode.commands.registerCommand('dvicode.reportIssue', () => reporter.reportIssue()),
    vscode.commands.registerCommand('dvicode.copyHierarchyPath', async (item: unknown) => {
      const p = hierarchyPath(item);
      if (p) {
        await vscode.env.clipboard.writeText(p);
      }
    }),
    vscode.workspace.onDidChangeConfiguration(() => offerRestart(ctx)),
  );
  const api: DvicodeApi = {
    ready: () => ensureClient(ctx),
    target: () => launch?.target,
    toServer: (uri) => launch?.mapper.toServer(uri),
    verification: () => views.verification,
    views: () => views,
    structure: () => structure,
    lastInternalError: () => reporter.last,
    issueBody: () => reporter.issueBody(),
  };
  try {
    await ensureClient(ctx);
  } catch {
    // Reported in startClient.
  }
  return api;
}

export async function deactivate(): Promise<void> {
  if (client) {
    await stopClient(client);
  }
  watcher?.dispose();
}
