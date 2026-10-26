// The Assertions and Coverage view: the assertions, covers and coverage items that use the
// declaration at the cursor (`dvicode/checksOf`), asked for on demand, grouped, with the
// properties, ports and covergroup arguments they are reached through.

import * as vscode from 'vscode';
import { Conn } from './views';
import { CHECK_GROUPS, CheckInfo, checkDescription, checkLabel, checkTooltip, checksMessage, valuesOnly } from './viewmodel';

interface CheckHit extends CheckInfo {
  uri: string | null;
  character: number | null;
  children: CheckHit[];
}

/** `dvicode/checksOf`. */
export interface ChecksResult {
  revision: number;
  target: { name: string; kind: string; uri: string | null; line: number | null; character: number | null };
  hits: CheckHit[];
  limits: string[];
  notes: string[];
}

const ICONS: Record<string, string> = {
  assert: 'pass',
  assume: 'question',
  restrict: 'circle-slash',
  expect: 'pass',
  cover: 'eye',
  'cover sequence': 'eye',
  coverpoint: 'graph-line',
  cross: 'graph',
  'covergroup event': 'watch',
  'default clocking': 'watch',
  'default disable': 'debug-disconnect',
  property: 'symbol-function',
  sequence: 'symbol-function',
  let: 'symbol-function',
  port: 'plug',
  argument: 'symbol-parameter',
};

type Node = { group: string; title: string; hits: CheckHit[] } | { hit: CheckHit };

export class ChecksView implements vscode.TreeDataProvider<Node>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  readonly view: vscode.TreeView<Node>;
  private readonly subs: vscode.Disposable[] = [];
  /** The last result shown. */
  result: ChecksResult | undefined;
  /** The newest analysis revision known: the result is out of date when it is older. */
  private latest = -1;
  /** The server restarted, or built the project again, since the result was made (its
   * revisions count anew). */
  private restarted = false;
  /** The engine the result came from: another one's paths are not its files. */
  private from: string | undefined;
  /** Hide what uses the target only as a clock or a reset. */
  valuesOnly = false;
  /** Request generation: an older answer is dropped. */
  private gen = 0;
  /** The search running. */
  private search: vscode.CancellationTokenSource | undefined;

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly conn: Conn,
  ) {
    this.view = vscode.window.createTreeView('dvicode.checks', { treeDataProvider: this, showCollapseAll: true });
    this.valuesOnly = ctx.workspaceState.get<boolean>('dvicode.checks.valuesOnly') === true;
    this.subs.push(
      this.view,
      vscode.commands.registerCommand('dvicode.showChecks', () => this.showAtCursor()),
      vscode.commands.registerCommand('dvicode.checks.refresh', () => this.refresh()),
      vscode.commands.registerCommand('dvicode.checks.valuesOnly', () => this.setValuesOnly(true)),
      vscode.commands.registerCommand('dvicode.checks.allRoles', () => this.setValuesOnly(false)),
    );
    void vscode.commands.executeCommand('setContext', 'dvicode.checks.valuesOnly', this.valuesOnly);
    this.present();
  }

  dispose(): void {
    for (const s of this.subs) {
      s.dispose();
    }
  }

  /** The analysis changed: the result shown may be out of date. */
  onRevision(revision?: number): void {
    if (revision !== undefined) {
      this.latest = Math.max(this.latest, revision);
    }
    this.present();
  }

  /** The server stopped or started: the result is kept, marked out of date. */
  reset(): void {
    this.gen++;
    this.restarted = this.result !== undefined;
    this.latest = -1;
    this.present();
  }

  /** The project was built again: the result is kept, marked out of date. */
  rebuilt(): void {
    this.restarted ||= this.result !== undefined;
    this.gen++;
    this.search?.cancel();
    this.present();
  }

  /** Drops a result of another engine than the one connected now. */
  private ofThisEngine(): void {
    const now = this.conn.endpoint();
    if (this.result && now !== undefined && this.from !== now) {
      this.result = undefined;
      this.emitter.fire(undefined);
    }
  }

  private setValuesOnly(on: boolean): void {
    this.valuesOnly = on;
    void this.ctx.workspaceState.update('dvicode.checks.valuesOnly', on);
    void vscode.commands.executeCommand('setContext', 'dvicode.checks.valuesOnly', on);
    this.present();
    this.emitter.fire(undefined);
  }

  /** Asks for the declaration at the active editor's cursor. */
  async showAtCursor(): Promise<void> {
    const ed = vscode.window.activeTextEditor;
    const m = this.conn.mapper();
    if (!ed || !m) {
      return;
    }
    const found = await this.ask(m.toServer(ed.document.uri), ed.selection.active);
    if (found === null) {
      void vscode.window.showInformationMessage('DVICode: put the cursor on a signal, variable, port, field or property to see its assertions and coverage.');
      return;
    }
    if (found) {
      await vscode.commands.executeCommand('dvicode.checks.focus');
    }
  }

  /** Asks again for the target shown, at its declaration (not at the old cursor position). */
  async refresh(): Promise<void> {
    this.ofThisEngine();
    const t = this.result?.target;
    if (!t?.uri || t.line === null || t.character === null) {
      await this.showAtCursor();
      return;
    }
    const name = t.name;
    const found = await this.ask(t.uri, new vscode.Position(t.line, t.character), name);
    if (found === null) {
      void vscode.window.showInformationMessage(`DVICode: '${name}' is no longer declared there; run Show Assertions and Coverage again.`);
    }
  }

  /** Asks `dvicode/checksOf` at a position (`expect`: the name the target must have). Whether
   * a result came (null: nothing searchable there; false: no answer). */
  private async ask(uri: string, position: vscode.Position, expect?: string): Promise<boolean | null> {
    const c = this.conn.client();
    if (!c) {
      return false;
    }
    const gen = ++this.gen;
    // A view's progress has no cancel button: a new search cancels the one running.
    this.search?.cancel();
    this.search?.dispose();
    const search = new vscode.CancellationTokenSource();
    this.search = search;
    let r: ChecksResult | null;
    try {
      r = await vscode.window.withProgress({ location: { viewId: 'dvicode.checks' }, title: 'Searching assertions and coverage' }, () =>
        c.sendRequest<ChecksResult | null>('dvicode/checksOf', { textDocument: { uri }, position }, search.token),
      );
    } catch (e) {
      // A cancelled search says nothing (the server answers RequestCancelled, -32800).
      const code = (e as { code?: number }).code;
      if (gen === this.gen && !search.token.isCancellationRequested && code !== -32800) {
        void vscode.window.showErrorMessage(`DVICode: ${e instanceof Error ? e.message : String(e)}`);
      }
      return false;
    }
    if (gen !== this.gen || search.token.isCancellationRequested) {
      return false;
    }
    if (!r || (expect !== undefined && r.target.name !== expect)) {
      return null;
    }
    this.result = r;
    this.from = this.conn.endpoint();
    this.restarted = false;
    this.latest = Math.max(this.latest, r.revision);
    this.present();
    this.emitter.fire(undefined);
    return true;
  }

  private hits(): CheckHit[] {
    const hits = this.result?.hits ?? [];
    return this.valuesOnly ? valuesOnly(hits) : hits;
  }

  private present(): void {
    this.ofThisEngine();
    const r = this.result;
    if (!r) {
      this.view.description = undefined;
      this.view.message = 'Right-click a signal, field or property and choose Show Assertions and Coverage.';
      return;
    }
    const stale = this.restarted || r.revision < this.latest ? ' · out of date' : '';
    const shown = this.valuesOnly ? ' · values only' : '';
    this.view.description = `${r.target.name} (${r.target.kind})${shown}${stale}`;
    const visible = this.hits().length;
    this.view.message = checksMessage(r.target.name, visible, r.limits, r.notes, (r.hits?.length ?? 0) - visible);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    if ('group' in n) {
      const item = new vscode.TreeItem(n.title, vscode.TreeItemCollapsibleState.Expanded);
      item.description = String(n.hits.length);
      item.id = `group:${n.group}`;
      return item;
    }
    const h = n.hit;
    const item = new vscode.TreeItem(checkLabel(h), h.children.length ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None);
    item.description = checkDescription(h);
    item.tooltip = new vscode.MarkdownString(checkTooltip(h));
    item.iconPath = new vscode.ThemeIcon(ICONS[h.kind] ?? 'symbol-misc');
    const m = this.conn.mapper();
    if (h.uri && m && h.line !== null) {
      const pos = new vscode.Position(h.line, h.character ?? 0);
      item.command = { command: 'vscode.open', title: 'Open', arguments: [m.toClient(h.uri), { selection: new vscode.Range(pos, pos) }] };
    }
    return item;
  }

  getChildren(n?: Node): Node[] {
    if (!n) {
      this.ofThisEngine();
      const hits = this.hits();
      return CHECK_GROUPS.map(([group, title]) => ({ group, title, hits: hits.filter((h) => h.group === group) })).filter((g) => g.hits.length);
    }
    if ('group' in n) {
      return n.hits.map((hit) => ({ hit }));
    }
    return n.hit.children.map((hit) => ({ hit }));
  }
}
