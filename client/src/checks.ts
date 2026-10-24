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
  /** Hide what uses the target only as a clock or a reset. */
  valuesOnly = false;
  /** Request generation: an older answer is dropped. */
  private gen = 0;

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
    this.latest = Number.MAX_SAFE_INTEGER;
    this.present();
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
    let r: ChecksResult | null;
    try {
      r = await vscode.window.withProgress({ location: { viewId: 'dvicode.checks' }, title: 'Searching assertions and coverage', cancellable: true }, (_, token) =>
        c.sendRequest<ChecksResult | null>('dvicode/checksOf', { textDocument: { uri }, position }, token),
      );
    } catch (e) {
      if (gen === this.gen) {
        void vscode.window.showErrorMessage(`DVICode: ${e instanceof Error ? e.message : String(e)}`);
      }
      return false;
    }
    if (gen !== this.gen) {
      return false;
    }
    if (!r || (expect !== undefined && r.target.name !== expect)) {
      return null;
    }
    this.result = r;
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
    const r = this.result;
    if (!r) {
      this.view.description = undefined;
      this.view.message = 'Right-click a signal, field or property and choose Show Assertions and Coverage.';
      return;
    }
    const stale = r.revision < this.latest ? ' · out of date' : '';
    const shown = this.valuesOnly ? ' · values only' : '';
    this.view.description = `${r.target.name} (${r.target.kind})${shown}${stale}`;
    this.view.message = checksMessage(r.target.name, this.hits().length, r.limits, r.notes);
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
      const hits = this.hits();
      return CHECK_GROUPS.map(([group, title]) => ({ group, title, hits: hits.filter((h) => h.group === group) })).filter((g) => g.hits.length);
    }
    if ('group' in n) {
      return n.hits.map((hit) => ({ hit }));
    }
    return n.hit.children.map((hit) => ({ hit }));
  }
}
