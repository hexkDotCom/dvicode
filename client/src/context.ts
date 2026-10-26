// The compile-context report: how the file in the editor (or a Compiled Files item) is compiled,
// as a read-only Markdown document shown in the preview beside the editor.

import * as vscode from 'vscode';
import type { Mapper } from './pathmap';
import type { Conn } from './views';
import { compileContextMarkdown, type CompileContextInfo } from './viewmodel';

export const CONTEXT_SCHEME = 'dvicode-context';

/** A link target for an engine uri on this machine (a line as the `#L` fragment). */
function linkOf(m: Mapper, uri: string, line?: number): string {
  let u = m.toClient(uri);
  if (line !== undefined) {
    u = u.with({ fragment: `L${line + 1}` });
  }
  return `<${u.toString()}>`;
}

/** How an engine uri is shown: relative to the workspace when inside it. */
function nameOf(m: Mapper, uri: string): string {
  const u = m.toClient(uri);
  const rel = vscode.workspace.asRelativePath(u, false);
  return rel || u.path;
}

export class CompileContextReports implements vscode.TextDocumentContentProvider, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this.emitter.event;
  private readonly subs: vscode.Disposable[] = [];
  /** Counts the analysis changes: an answer asked for before the last one is asked again. */
  private generation = 0;
  /** The source of the report shown last (a preview has no text editor to ask). */
  private lastSource: vscode.Uri | undefined;

  constructor(private readonly conn: Conn) {
    this.subs.push(
      this.emitter,
      vscode.workspace.registerTextDocumentContentProvider(CONTEXT_SCHEME, this),
      vscode.commands.registerCommand('dvicode.showCompileContext', (arg?: unknown) => this.show(arg)),
      vscode.commands.registerCommand('dvicode.copyCompileContext', (arg?: unknown) => this.copy(arg)),
    );
  }

  dispose(): void {
    for (const s of this.subs) {
      s.dispose();
    }
  }

  /** The report document of a source file. */
  static reportUri(source: vscode.Uri): vscode.Uri {
    const base = source.path.split('/').pop() || 'file';
    return vscode.Uri.from({ scheme: CONTEXT_SCHEME, path: `/${base} (compile context).md`, query: source.toString() });
  }

  /** The source file a command is about: a uri, a Compiled Files item, else the editor's. */
  private sourceOf(arg: unknown): vscode.Uri | undefined {
    if (arg instanceof vscode.Uri) {
      return arg.scheme === CONTEXT_SCHEME ? vscode.Uri.parse(arg.query) : arg;
    }
    const item = arg as { resourceUri?: vscode.Uri } | undefined;
    if (item?.resourceUri instanceof vscode.Uri) {
      return item.resourceUri;
    }
    const doc = vscode.window.activeTextEditor?.document;
    if (doc?.uri.scheme === CONTEXT_SCHEME) {
      return vscode.Uri.parse(doc.uri.query);
    }
    return doc?.uri ?? this.lastSource;
  }

  async provideTextDocumentContent(uri: vscode.Uri, token: vscode.CancellationToken): Promise<string> {
    const asked = this.generation;
    const text = await this.render(vscode.Uri.parse(uri.query), token);
    if (asked !== this.generation && !token.isCancellationRequested) {
      // The analysis changed while this was answered (the document may not be open yet, so
      // `refresh` did not reach it): it is asked for again.
      setTimeout(() => this.emitter.fire(uri), 0);
    }
    return text;
  }

  private async render(source: vscode.Uri, token?: vscode.CancellationToken): Promise<string> {
    const c = this.conn.client();
    const m = this.conn.mapper();
    if (!c || !m) {
      return '# Compile context\n\nThe DVICode engine is not running.\n';
    }
    try {
      const r = await c.sendRequest<CompileContextInfo>('dvicode/compileContext', { textDocument: { uri: m.toServer(source) } }, token);
      return compileContextMarkdown(
        r,
        (u, line) => linkOf(m, u, line),
        (u) => nameOf(m, u),
      );
    } catch (e) {
      return `# Compile context\n\nNo answer from the engine: ${e instanceof Error ? e.message : String(e)}\n`;
    }
  }

  private async show(arg: unknown): Promise<void> {
    const source = this.sourceOf(arg);
    if (!source) {
      void vscode.window.showInformationMessage('DVICode: open a SystemVerilog file to see how it is compiled.');
      return;
    }
    this.lastSource = source;
    const doc = CompileContextReports.reportUri(source);
    // Asked again when shown again.
    this.emitter.fire(doc);
    await vscode.commands.executeCommand('markdown.showPreviewToSide', doc);
  }

  private async copy(arg: unknown): Promise<void> {
    const source = this.sourceOf(arg);
    if (!source) {
      return;
    }
    await vscode.env.clipboard.writeText(await this.render(source));
    void vscode.window.showInformationMessage('DVICode: compile context copied.');
  }

  /** The analysis changed: open reports are asked for again. */
  refresh(): void {
    this.generation++;
    for (const d of vscode.workspace.textDocuments) {
      if (d.uri.scheme === CONTEXT_SCHEME) {
        this.emitter.fire(d.uri);
      }
    }
  }
}
