// The Drivers and Loads view: the places that write (Show Drivers) or read (Show Loads) the
// signal at the cursor (`dvicode/accessesOf`), asked for on demand, grouped, with the port
// connections they are reached through.

import * as vscode from 'vscode';
import { Conn } from './views';
import { ACCESS_GROUPS, AccessInfo, accessDescription, accessLabel, accessTooltip, accessesMessage } from './viewmodel';

interface AccessSite extends AccessInfo {
  uri: string | null;
  character: number | null;
  children: AccessSite[];
}

/** `dvicode/accessesOf`. */
export interface AccessesResult {
  revision: number;
  target: { name: string; kind: string; uri: string | null; line: number | null; character: number | null };
  /** `writes` or `reads`. */
  mode: string;
  sites: AccessSite[];
  limits: string[];
  notes: string[];
}

const ICONS: Record<string, string> = {
  continuous: 'arrow-right',
  blocking: 'edit',
  nonblocking: 'edit',
  compound: 'edit',
  increment: 'edit',
  'procedural assign': 'edit',
  force: 'zap',
  release: 'debug-disconnect',
  deassign: 'debug-disconnect',
  'net initializer': 'symbol-constant',
  'variable initializer': 'symbol-constant',
  read: 'eye',
  argument: 'symbol-parameter',
  'gate output': 'circuit-board',
  'gate input': 'circuit-board',
  'switch terminal': 'circuit-board',
  alias: 'link',
  clocking: 'watch',
  port: 'plug',
  instance: 'plug',
};

type Node = { group: string; title: string; sites: AccessSite[] } | { site: AccessSite };

export class DriversView implements vscode.TreeDataProvider<Node>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  readonly view: vscode.TreeView<Node>;
  private readonly subs: vscode.Disposable[] = [];
  /** The last result shown. */
  result: AccessesResult | undefined;
  /** The newest analysis revision known: the result is out of date when it is older. */
  private latest = -1;
  /** The server restarted, or built the project again, since the result was made (its
   * revisions count anew). */
  private restarted = false;
  /** The engine the result came from: another one's paths are not its files. */
  private from: string | undefined;
  /** Request generation: an older answer is dropped. */
  private gen = 0;
  /** The search running. */
  private search: vscode.CancellationTokenSource | undefined;

  constructor(private readonly conn: Conn) {
    this.view = vscode.window.createTreeView('dvicode.accesses', { treeDataProvider: this, showCollapseAll: true });
    this.subs.push(
      this.view,
      vscode.commands.registerCommand('dvicode.showDrivers', () => this.showAtCursor('writes')),
      vscode.commands.registerCommand('dvicode.showLoads', () => this.showAtCursor('reads')),
      vscode.commands.registerCommand('dvicode.accesses.refresh', () => this.refresh()),
      vscode.commands.registerCommand('dvicode.accesses.showDrivers', () => this.refresh('writes')),
      vscode.commands.registerCommand('dvicode.accesses.showLoads', () => this.refresh('reads')),
    );
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

  /** Asks for the signal at the active editor's cursor. */
  async showAtCursor(mode: string): Promise<void> {
    const ed = vscode.window.activeTextEditor;
    const m = this.conn.mapper();
    if (!ed || !m) {
      return;
    }
    const found = await this.ask(m.toServer(ed.document.uri), ed.selection.active, mode);
    if (found === null) {
      void vscode.window.showInformationMessage(`DVICode: put the cursor on a signal, variable or port to see its ${mode === 'writes' ? 'drivers' : 'loads'}.`);
      return;
    }
    if (found) {
      await vscode.commands.executeCommand('dvicode.accesses.focus');
    }
  }

  /** Asks again for the target shown (in `mode`, else the mode shown), at its declaration. */
  async refresh(mode?: string): Promise<void> {
    this.ofThisEngine();
    const t = this.result?.target;
    const m = mode ?? this.result?.mode ?? 'writes';
    if (!t?.uri || t.line === null || t.character === null) {
      await this.showAtCursor(m);
      return;
    }
    const name = t.name;
    const found = await this.ask(t.uri, new vscode.Position(t.line, t.character), m, name);
    if (found === null) {
      void vscode.window.showInformationMessage(`DVICode: '${name}' is no longer declared there; run Show Drivers or Show Loads again.`);
    }
  }

  /** Asks `dvicode/accessesOf` at a position (`expect`: the name the target must have).
   * Whether a result came (null: no signal there; false: no answer). */
  private async ask(uri: string, position: vscode.Position, mode: string, expect?: string): Promise<boolean | null> {
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
    let r: AccessesResult | null;
    try {
      const title = mode === 'writes' ? 'Searching drivers' : 'Searching loads';
      r = await vscode.window.withProgress({ location: { viewId: 'dvicode.accesses' }, title }, () =>
        c.sendRequest<AccessesResult | null>('dvicode/accessesOf', { textDocument: { uri }, position, mode }, search.token),
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
    void vscode.commands.executeCommand('setContext', 'dvicode.accesses.mode', r.mode);
    this.present();
    this.emitter.fire(undefined);
    return true;
  }

  private present(): void {
    this.ofThisEngine();
    const r = this.result;
    if (!r) {
      this.view.description = undefined;
      this.view.message = 'Right-click a signal, variable or port and choose Show Drivers or Show Loads.';
      return;
    }
    const stale = this.restarted || r.revision < this.latest ? ' · out of date' : '';
    this.view.description = `${r.mode === 'writes' ? 'drivers' : 'loads'} of ${r.target.name} (${r.target.kind})${stale}`;
    this.view.message = accessesMessage(r.target.name, r.mode, r.sites.length, r.limits, r.notes);
  }

  getTreeItem(n: Node): vscode.TreeItem {
    if ('group' in n) {
      const item = new vscode.TreeItem(n.title, vscode.TreeItemCollapsibleState.Expanded);
      item.description = String(n.sites.length);
      item.id = `group:${n.group}`;
      return item;
    }
    const s = n.site;
    const item = new vscode.TreeItem(accessLabel(s), s.children.length ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None);
    item.description = accessDescription(s);
    item.tooltip = new vscode.MarkdownString(accessTooltip(s));
    item.iconPath = new vscode.ThemeIcon(ICONS[s.kind] ?? 'symbol-misc');
    const m = this.conn.mapper();
    if (s.uri && m && s.line !== null) {
      const pos = new vscode.Position(s.line, s.character ?? 0);
      item.command = { command: 'vscode.open', title: 'Open', arguments: [m.toClient(s.uri), { selection: new vscode.Range(pos, pos) }] };
    }
    return item;
  }

  getChildren(n?: Node): Node[] {
    if (!n) {
      this.ofThisEngine();
      const sites = this.result?.sites ?? [];
      return ACCESS_GROUPS.map(([group, title]) => ({ group, title, sites: sites.filter((s) => s.group === group) })).filter((g) => g.sites.length);
    }
    if ('group' in n) {
      return n.sites.map((site) => ({ site }));
    }
    return n.site.children.map((site) => ({ site }));
  }
}
