// Clickable simulator logs: source locations and UVM component paths in log files and in
// terminals (see logparse.ts for the forms read). A link is resolved when clicked, by the
// engine (`dvicode/findFile`, `dvicode/component`): a log's paths are often from another
// machine, so a file is found by the end of its path among the project's files.

import * as vscode from 'vscode';
import { Conn } from './views';
import { LinkScanner, LogLink, lineLinks, logTest } from './logparse';

/** Links listed per document at most. */
const MAX_LINKS = 20_000;
/** Lines read between two yields of the extension host's thread. */
const LINES_PER_STEP = 20_000;
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
}

interface OpenArgs {
  path: string;
  line: number;
  /** The log the link is in (its folder is where relative paths start). */
  log?: string;
}

interface ComponentArgs {
  path: string;
  log?: string;
}

interface LogTerminalLink extends vscode.TerminalLink {
  link: LogLink;
}

export class LogLinks implements vscode.Disposable {
  private readonly subs: vscode.Disposable[] = [];

  constructor(
    private readonly conn: Conn,
    /** Selects a component in the Verification Hierarchy; whether it was shown. */
    private readonly reveal: (hit: ComponentHit) => Promise<boolean>,
  ) {
    this.subs.push(
      vscode.languages.registerDocumentLinkProvider(SELECTOR, { provideDocumentLinks: (d, t) => this.documentLinks(d, t) }),
      vscode.window.registerTerminalLinkProvider({
        provideTerminalLinks: (c) => this.terminalLinks(c.line),
        handleTerminalLink: (l: LogTerminalLink) => this.follow(l.link, undefined),
      }),
      vscode.commands.registerCommand('dvicode.logs.open', (a: OpenArgs) => this.open(a)),
      vscode.commands.registerCommand('dvicode.logs.component', (a: ComponentArgs) => this.component(a)),
    );
  }

  dispose(): void {
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
    const maxMB = vscode.workspace.getConfiguration('dvicode').get<number>('logs.maxFileSizeMB', 50);
    const text = doc.getText();
    if (text.length > maxMB * 1_000_000) {
      return [];
    }
    const scan = new LinkScanner(text, MAX_LINKS);
    while (!scan.step(LINES_PER_STEP)) {
      await new Promise((r) => setImmediate(r));
      if (token.isCancellationRequested) {
        return [];
      }
    }
    const log = doc.uri.toString();
    return scan.links.map((l) => {
      const range = new vscode.Range(doc.positionAt(l.start), doc.positionAt(l.end));
      const [command, args, tooltip] = l.kind === 'file' ? ['dvicode.logs.open', { path: l.path, line: l.line, log }, `Open ${l.path}:${l.line}`] : ['dvicode.logs.component', { path: l.path, log }, `Show ${l.path} in the Verification Hierarchy`];
      const link = new vscode.DocumentLink(range, vscode.Uri.parse(`command:${command}?${encodeURIComponent(JSON.stringify([args]))}`));
      link.tooltip = `${tooltip} (DVICode)`;
      return link;
    });
  }

  private terminalLinks(line: string): LogTerminalLink[] {
    if (!this.enabled()) {
      return [];
    }
    return lineLinks(line).map((l) => ({
      startIndex: l.start,
      length: l.end - l.start,
      tooltip: l.kind === 'file' ? `Open ${l.path}:${l.line} (DVICode)` : `Show ${l.path} in the Verification Hierarchy (DVICode)`,
      link: l,
    }));
  }

  private follow(l: LogLink, log: string | undefined): Promise<void> {
    return l.kind === 'file' ? this.open({ path: l.path, line: l.line, log }) : this.component({ path: l.path, log });
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

  /** The files a log's path may name: by the engine, or without one, the path itself or
   * relative to the log's folder. */
  private async candidates(a: OpenArgs): Promise<vscode.Uri[]> {
    const c = this.conn.client();
    const m = this.conn.mapper();
    const folder = a.log ? vscode.Uri.joinPath(vscode.Uri.parse(a.log), '..') : undefined;
    if (c?.isRunning() && m) {
      const base = folder ? m.toServer(folder) : null;
      const r = await c.sendRequest<{ files: string[] }>('dvicode/findFile', { path: a.path, base }).catch(() => undefined);
      if (r) {
        return r.files.map((f) => m.toClient(f));
      }
    }
    const tries = [vscode.Uri.file(a.path), ...(folder ? [vscode.Uri.joinPath(folder, a.path)] : [])];
    for (const u of tries) {
      const s = await Promise.resolve(vscode.workspace.fs.stat(u)).catch(() => undefined);
      if (s && s.type & vscode.FileType.File) {
        return [u];
      }
    }
    return [];
  }

  /** Selects a component a log names in the Verification Hierarchy, and opens its class. */
  async component(a: ComponentArgs): Promise<void> {
    const c = this.conn.client();
    if (!c?.isRunning()) {
      void vscode.window.showInformationMessage('DVICode: the engine is not running.');
      return;
    }
    const doc = a.log ? vscode.workspace.textDocuments.find((d) => d.uri.toString() === a.log) : undefined;
    const test = doc ? logTest(doc.getText()) : undefined;
    const r = await c.sendRequest<{ hit: ComponentHit | null }>('dvicode/component', { path: a.path, test: test ?? null }).catch(() => undefined);
    const hit = r?.hit;
    if (!hit) {
      void vscode.window.showInformationMessage(`DVICode: ${a.path} is not a component of the Verification Hierarchy.`);
      return;
    }
    await this.reveal(hit);
    const m = this.conn.mapper();
    if (hit.uri && m) {
      const pos = new vscode.Position(hit.line, 0);
      await vscode.window.showTextDocument(m.toClient(hit.uri), { selection: new vscode.Range(pos, pos), preserveFocus: false });
    }
    if (hit.matched < hit.segments) {
      const parts = a.path.split('.');
      void vscode.window.showInformationMessage(
        `DVICode: ${hit.test}'s Verification Hierarchy has ${parts.slice(0, hit.matched).join('.')} but not ${parts.slice(hit.matched).join('.')}.`,
      );
    }
  }
}
