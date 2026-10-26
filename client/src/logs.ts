// Clickable simulator logs: source locations and UVM component paths in log files and in
// terminals (see logparse.ts for the forms read). A link is resolved when clicked, by the
// engine (`dvicode/findFile`, `dvicode/component`): a log's paths are often from another
// machine, so a file is found by the end of its path among the project's files.

import * as vscode from 'vscode';
import { Conn } from './views';
import { LinkScanner, LogLink, lineLinks, locationLine, logTest } from './logparse';

/** Links listed per document at most. */
const MAX_LINKS = 20_000;
/** Lines, and bytes, read between two yields of the extension host's thread. */
const LINES_PER_STEP = 20_000;
const BYTES_PER_STEP = 1_000_000;
/** Log files: the editor's Log language, `*.log`, and Questa's `transcript`. */
const SELECTOR: vscode.DocumentSelector = [{ language: 'log' }, { pattern: '**/*.log' }, { pattern: '**/transcript' }];
/** Languages that have links of their own. */
const NOT_LOGS = new Set(['systemverilog', 'verilog', 'dvicode-build']);

/** `dvicode/component`: the component a log names, as the Verification Hierarchy keys it. */
export interface ComponentHit {
  test: string;
  names: string[];
  matched: number;
  segments: number;
  class: string;
  uri: string | null;
  line: number;
  /** The test the log names, when the build has none of that name (another one was searched). */
  unknown_test: string | null;
  /** The tree was cut by its size before the rest of the path. */
  incomplete: boolean;
  /** The analysis revision of the answer (set by the client). */
  revision?: number;
}

interface OpenArgs {
  path: string;
  line: number;
  /** The log the link is in (its folder is where relative paths start). */
  log?: string;
  /** A terminal's working folder (where relative paths start). */
  cwd?: string;
}

interface ComponentArgs {
  path: string;
  log?: string;
  /** Where the link is in the log (the test is the last one announced before it). */
  at?: number;
}

interface LogTerminalLink extends vscode.TerminalLink {
  link: LogLink;
  cwd?: string;
}

/** A terminal's working folder, when its shell reports it. */
function terminalCwd(t: vscode.Terminal): string | undefined {
  const cwd = (t as { shellIntegration?: { cwd?: vscode.Uri } }).shellIntegration?.cwd;
  return cwd?.toString();
}

export class LogLinks implements vscode.Disposable {
  private readonly subs: vscode.Disposable[] = [];

  constructor(
    private readonly conn: Conn,
    /** Selects a component in the Verification Hierarchy (while `wanted`); whether it was shown. */
    private readonly reveal: (hit: ComponentHit, wanted: () => boolean) => Promise<boolean>,
    /** The test the Verification Hierarchy shows (undefined: all tests, or none yet). */
    private readonly viewTest: () => string | undefined,
  ) {
    this.provider = this.registerProvider();
    this.subs.push(
      vscode.window.registerTerminalLinkProvider({
        provideTerminalLinks: (c) => this.terminalLinks(c.line, terminalCwd(c.terminal)),
        handleTerminalLink: (l: LogTerminalLink) => (l.link.kind === 'file' ? this.open({ path: l.link.path, line: l.link.line, cwd: l.cwd }) : this.component({ path: l.link.path })),
      }),
      vscode.commands.registerCommand('dvicode.logs.open', (a: OpenArgs) => this.open(a)),
      vscode.commands.registerCommand('dvicode.logs.component', (a: ComponentArgs) => this.component(a)),
      // A changed setting: the links shown are made again (none when turned off).
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('dvicode.logs')) {
          this.provider.dispose();
          this.provider = this.registerProvider();
        }
      }),
    );
  }

  /** The document link provider (registered again to drop the links shown). */
  private provider: vscode.Disposable;
  /** Changes with each component link followed: an older search's answer is dropped. */
  private componentGen = 0;

  private registerProvider(): vscode.Disposable {
    return vscode.languages.registerDocumentLinkProvider(SELECTOR, { provideDocumentLinks: (d, t) => this.documentLinks(d, t) });
  }

  dispose(): void {
    this.provider.dispose();
    for (const s of this.subs) {
      s.dispose();
    }
  }

  private enabled(): boolean {
    return vscode.workspace.getConfiguration('dvicode').get<boolean>('logs.links', true);
  }

  /** A log's links, read a part at a time (the editor asks again each time the log grows, and
   * cancels the reading under way); none for a log above `dvicode.logs.maxFileSizeMB`. */
  private async documentLinks(doc: vscode.TextDocument, token: vscode.CancellationToken): Promise<vscode.DocumentLink[]> {
    if (!this.enabled() || NOT_LOGS.has(doc.languageId)) {
      return [];
    }
    // The size from the document's end, before its text is taken (memory 9).
    const maxMB = vscode.workspace.getConfiguration('dvicode').get<number>('logs.maxFileSizeMB', 50);
    const size = doc.offsetAt(doc.lineAt(doc.lineCount - 1).range.end);
    if (size > maxMB * 1_000_000 || token.isCancellationRequested) {
      return [];
    }
    const text = doc.getText();
    const scan = new LinkScanner(text, MAX_LINKS);
    while (!scan.step(LINES_PER_STEP, BYTES_PER_STEP)) {
      await new Promise((r) => setImmediate(r));
      if (token.isCancellationRequested) {
        return [];
      }
    }
    const log = doc.uri.toString();
    return scan.links.map((l) => {
      const range = new vscode.Range(doc.positionAt(l.start), doc.positionAt(l.end));
      const [command, args, tooltip] = l.kind === 'file' ? ['dvicode.logs.open', { path: l.path, line: l.line, log }, `Open ${l.path}:${l.line}`] : ['dvicode.logs.component', { path: l.path, log, at: l.start }, `Show ${l.path} in the Verification Hierarchy`];
      const link = new vscode.DocumentLink(range, vscode.Uri.parse(`command:${command}?${encodeURIComponent(JSON.stringify([args]))}`));
      link.tooltip = `${tooltip} (DVICode)`;
      return link;
    });
  }

  /** A terminal line's links; a VCS location alone on its line too (its header is the line
   * before, which a terminal does not give). */
  private terminalLinks(line: string, cwd: string | undefined): LogTerminalLink[] {
    if (!this.enabled()) {
      return [];
    }
    const found = lineLinks(line);
    const alone = found.length ? undefined : locationLine(line);
    return (alone ? [alone] : found).map((l) => ({
      startIndex: l.start,
      length: l.end - l.start,
      tooltip: l.kind === 'file' ? `Open ${l.path}:${l.line} (DVICode)` : `Show ${l.path} in the Verification Hierarchy (DVICode)`,
      link: l,
      cwd,
    }));
  }

  /** Opens the source a log names, at its line. */
  async open(a: OpenArgs): Promise<void> {
    const files = await this.candidates(a);
    let target: vscode.Uri | undefined = files[0];
    if (files.length > 1) {
      const pick = await vscode.window.showQuickPick(
        files.map((u) => ({ label: vscode.workspace.asRelativePath(u), uri: u })),
        { placeHolder: `${a.path} matches several files of the project` },
      );
      if (!pick) {
        return;
      }
      target = pick.uri;
    }
    if (!target) {
      void vscode.window.showInformationMessage(`DVICode: ${a.path} is not a file of the project.`);
      return;
    }
    const pos = new vscode.Position(Math.max(0, a.line - 1), 0);
    await vscode.window.showTextDocument(target, { selection: new vscode.Range(pos, pos) });
  }

  /** The files a log's path may name: the path as written when it exists here (an absolute
   * one on the log's own file system too: a remote or WSL folder), relative to the log's folder
   * and to each workspace folder; else what the engine finds, on its machine and by the end of
   * the path among the project's files. */
  private async candidates(a: OpenArgs): Promise<vscode.Uri[]> {
    const logUri = a.log ? vscode.Uri.parse(a.log) : undefined;
    const folder = logUri ? vscode.Uri.joinPath(logUri, '..') : undefined;
    const drive = /^[A-Za-z]:[\\/]/.test(a.path);
    const tries: vscode.Uri[] = [];
    if (drive || /^[\\/]/.test(a.path)) {
      if (logUri && !drive && logUri.scheme !== 'file') {
        tries.push(logUri.with({ path: a.path.replace(/\\/g, '/'), query: '', fragment: '' }));
      }
      tries.push(vscode.Uri.file(a.path));
    } else {
      const cwd = a.cwd ? [vscode.Uri.parse(a.cwd)] : [];
      for (const base of [...cwd, ...(folder ? [folder] : []), ...(vscode.workspace.workspaceFolders ?? []).map((f) => f.uri)]) {
        tries.push(vscode.Uri.joinPath(base, a.path.replace(/\\/g, '/')));
      }
    }
    for (const u of tries) {
      const s = await Promise.resolve(vscode.workspace.fs.stat(u)).catch(() => undefined);
      if (s && s.type & vscode.FileType.File) {
        return [u];
      }
    }
    const c = this.conn.client();
    const m = this.conn.mapper();
    if (c?.isRunning() && m) {
      const base = folder ? m.toServer(folder) : null;
      const r = await c.sendRequest<{ files: string[] }>('dvicode/findFile', { path: a.path, base }).catch(() => undefined);
      return r ? r.files.map((f) => m.toClient(f)) : [];
    }
    return [];
  }

  /** Selects a component a log names in the Verification Hierarchy, and opens its class. */
  async component(a: ComponentArgs): Promise<void> {
    const c = this.conn.client();
    const m = this.conn.mapper();
    if (!c?.isRunning() || !m) {
      void vscode.window.showInformationMessage('DVICode: the engine is not running.');
      return;
    }
    // A later click wins: an earlier search that answers after it is dropped.
    const gen = ++this.componentGen;
    const doc = a.log ? vscode.workspace.textDocuments.find((d) => d.uri.toString() === a.log) : undefined;
    // The test the log ran (the last announced before the link); in a terminal (or a log that
    // does not say), the view's.
    const test = (doc ? logTest(doc.getText(), a.at ?? Infinity) : undefined) ?? this.viewTest();
    const r = await c.sendRequest<{ revision: number; hit: ComponentHit | null }>('dvicode/component', { path: a.path, test: test ?? null }).catch(() => undefined);
    if (gen !== this.componentGen || this.conn.client() !== c) {
      return;
    }
    const hit = r?.hit;
    if (!hit) {
      void vscode.window.showInformationMessage(`DVICode: ${a.path} is not a component of the Verification Hierarchy.`);
      return;
    }
    await this.reveal({ ...hit, revision: r.revision }, () => gen === this.componentGen);
    if (gen !== this.componentGen) {
      return;
    }
    if (hit.uri) {
      const pos = new vscode.Position(hit.line, 0);
      await vscode.window.showTextDocument(m.toClient(hit.uri), { selection: new vscode.Range(pos, pos), preserveFocus: false });
    }
    const parts = a.path.split('.');
    const reached = parts.slice(0, hit.matched).join('.');
    const rest = parts.slice(hit.matched).join('.');
    const notes: string[] = [];
    if (hit.unknown_test) {
      notes.push(`the log ran ${hit.unknown_test}, which is not a test of this build: ${hit.test}'s hierarchy is shown`);
    }
    if (hit.matched < hit.segments) {
      notes.push(hit.incomplete ? `the hierarchy is too large to look past ${reached} for ${rest}` : `${hit.test}'s hierarchy has ${reached} but not ${rest}`);
    }
    if (notes.length) {
      void vscode.window.showInformationMessage(`DVICode: ${notes.join('; ')}.`);
    }
  }
}
