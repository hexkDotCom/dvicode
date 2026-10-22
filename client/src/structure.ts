// The Structure view: the declarations of the active editor's file, grouped by what they are
// (`dvicode/structure`), with a filter, following the cursor.

import * as vscode from 'vscode';
import { Conn, uriKey } from './views';
import { StructNode, innermostAt } from './viewmodel';

/** `dvicode/structure`. */
interface StructureResult {
  revision: number;
  version: number | null;
  status: 'ok' | 'not-compiled' | 'none';
  context?: string;
  others?: number;
  incomplete?: boolean;
  nodes: StructNode[];
}

const ICONS: Record<string, string> = {
  module: 'symbol-module',
  interface: 'symbol-interface',
  program: 'symbol-module',
  checker: 'symbol-module',
  package: 'package',
  class: 'symbol-class',
  'interface class': 'symbol-interface',
  method: 'symbol-method',
  phase: 'symbol-event',
  function: 'symbol-function',
  task: 'symbol-function',
  field: 'symbol-field',
  signal: 'symbol-variable',
  port: 'plug',
  parameter: 'symbol-constant',
  typedef: 'symbol-type-parameter',
  constraint: 'symbol-ruler',
  covergroup: 'graph',
  coverpoint: 'graph-line',
  instance: 'symbol-object',
  generate: 'symbol-namespace',
  modport: 'symbol-interface',
  clocking: 'watch',
  assertion: 'check',
  process: 'pulse',
  include: 'file-symlink-file',
  define: 'symbol-key',
  import: 'package',
  inactive: 'circle-slash',
  base: 'symbol-class',
  group: 'list-tree',
};

class Item extends vscode.TreeItem {
  kids: Item[] = [];
  constructor(
    readonly node: StructNode,
    readonly parent: Item | undefined,
    state: vscode.TreeItemCollapsibleState,
  ) {
    super(node.name, state);
  }
}

/** What the view remembers (workspaceState). */
interface Remembered {
  filter?: string;
  sync?: boolean;
}

const STATE = 'dvicode.view.structure';

export class Structure implements vscode.TreeDataProvider<Item>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<Item | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  readonly view: vscode.TreeView<Item>;
  private readonly subs: vscode.Disposable[] = [];
  private roots: Item[] = [];
  private items = new Map<string, Item>();
  private result: StructureResult | undefined;
  /** The document shown (its engine URI), and the request generation: older answers are dropped. */
  private uri: string | undefined;
  private gen = 0;
  private error: string | undefined;
  private filter: string | undefined;
  private sync = true;
  private timer: NodeJS.Timeout | undefined;
  private followTimer: NodeJS.Timeout | undefined;
  private disposed = false;

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly conn: Conn,
  ) {
    this.view = vscode.window.createTreeView('dvicode.structure', { treeDataProvider: this, showCollapseAll: true });
    const r = ctx.workspaceState.get<Remembered>(STATE) ?? {};
    this.filter = r.filter || undefined;
    // Following the cursor is on unless turned off.
    this.sync = r.sync !== false;
    this.setContext();
    this.subs.push(
      this.view,
      vscode.window.onDidChangeActiveTextEditor(() => this.schedule(50)),
      vscode.workspace.onDidChangeTextDocument((e) => {
        if (e.document === vscode.window.activeTextEditor?.document) {
          this.schedule(300);
        }
      }),
      vscode.window.onDidChangeTextEditorSelection((e) => {
        if (e.textEditor === vscode.window.activeTextEditor && this.sync) {
          clearTimeout(this.followTimer);
          this.followTimer = setTimeout(() => void this.follow(), 200);
        }
      }),
      this.view.onDidChangeVisibility((e) => {
        if (e.visible) {
          this.schedule(0);
        }
      }),
      vscode.commands.registerCommand('dvicode.structure.filter', async () => {
        const text = await vscode.window.showInputBox({ title: 'Filter Structure', value: this.filter ?? '', prompt: 'Text anywhere in a name; * and ? as wildcards. Empty: no filter.' });
        if (text !== undefined) {
          this.setFilter(text);
        }
      }),
      vscode.commands.registerCommand('dvicode.structure.clearFilter', () => this.setFilter(undefined)),
      vscode.commands.registerCommand('dvicode.structure.syncOn', () => this.setSync(true)),
      vscode.commands.registerCommand('dvicode.structure.syncOff', () => this.setSync(false)),
    );
    this.schedule(0);
  }

  dispose(): void {
    this.gen++;
    this.disposed = true;
    clearTimeout(this.timer);
    clearTimeout(this.followTimer);
    this.emitter.dispose();
    for (const s of this.subs) {
      s.dispose();
    }
  }

  /** The analysis changed (an edit, a build): the tree again. */
  onRevision(): void {
    this.schedule(300);
  }

  /** The server stopped or started. */
  reset(): void {
    this.gen++;
    this.result = undefined;
    this.schedule(0);
  }

  getTreeItem(e: Item): vscode.TreeItem {
    return e;
  }

  getParent(e: Item): Item | undefined {
    return e.parent;
  }

  getChildren(e?: Item): Item[] {
    return e ? e.kids : this.roots;
  }

  private schedule(ms: number): void {
    // An answer to a request made before this is no longer wanted.
    this.gen++;
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.load(), ms);
  }

  private setFilter(text: string | undefined): void {
    this.filter = text?.trim() || undefined;
    this.save();
    void this.load();
  }

  private setSync(on: boolean): void {
    this.sync = on;
    this.save();
    if (on) {
      void this.follow();
    }
  }

  private save(): void {
    void this.ctx.workspaceState.update(STATE, { filter: this.filter, sync: this.sync } satisfies Remembered);
    this.setContext();
  }

  private setContext(): void {
    void vscode.commands.executeCommand('setContext', 'dvicode.structure.filtered', this.filter !== undefined);
    void vscode.commands.executeCommand('setContext', 'dvicode.structure.sync', this.sync);
  }

  /** The active editor's document, when the engine knows it (its engine URI). */
  private document(): { doc: vscode.TextDocument; uri: string } | undefined {
    const ed = vscode.window.activeTextEditor;
    const m = this.conn.mapper();
    if (!ed || !m || !['systemverilog', 'verilog'].includes(ed.document.languageId)) {
      return undefined;
    }
    const u = ed.document.uri;
    try {
      // A dvicode:// document of another host is not this engine's.
      if (u.scheme === 'dvicode' && m.toClient(m.toServer(u)).authority.toLowerCase() !== u.authority.toLowerCase()) {
        return undefined;
      }
      if (u.scheme !== 'file' && u.scheme !== 'dvicode') {
        return undefined;
      }
      return { doc: ed.document, uri: m.toServer(u) };
    } catch {
      return undefined;
    }
  }

  private async load(): Promise<void> {
    if (!this.view.visible) {
      return;
    }
    const gen = ++this.gen;
    const c = this.conn.client();
    const d = this.document();
    if (!d) {
      // Keep showing the last file while another kind of editor is active.
      if (!this.result) {
        this.present();
      }
      return;
    }
    if (!c) {
      this.result = undefined;
      this.roots = [];
      this.present();
      this.emitter.fire(undefined);
      return;
    }
    try {
      const r = await c.sendRequest<StructureResult>('dvicode/structure', { textDocument: { uri: d.uri }, filter: this.filter ?? null });
      // Stale: a newer request, another editor, or the view went away.
      if (gen !== this.gen || this.disposed || this.document()?.uri !== d.uri) {
        return;
      }
      // An answer for an older text of the document: another comes after the engine updates.
      if (r.version !== null && r.version !== undefined && r.version < d.doc.version) {
        return;
      }
      this.result = r;
      this.uri = d.uri;
      this.error = undefined;
      this.setTree(r.nodes);
    } catch (e) {
      if (gen !== this.gen) {
        return;
      }
      this.error = e instanceof Error ? e.message : String(e);
      this.roots = [];
      this.items.clear();
    }
    this.present();
    this.emitter.fire(undefined);
    if (this.sync) {
      void this.follow();
    }
  }

  private setTree(nodes: StructNode[]): void {
    this.items.clear();
    const m = this.conn.mapper();
    const make = (ns: StructNode[], parent: Item | undefined, depth: number): Item[] =>
      ns.map((n) => {
        const open = n.children.length === 0 ? vscode.TreeItemCollapsibleState.None : this.filter || (!n.collapsed && (n.kind === 'group' || depth === 0)) ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed;
        const item = new Item(n, parent, open);
        item.id = `structure:${this.uri ?? ''}:${this.filter ? 'f:' : ''}${n.id}`;
        const extra = [n.detail, n.mark, n.inherited_from && n.kind !== 'group' ? `from ${n.inherited_from}` : undefined].filter((x) => x);
        item.description = extra.join(' · ') || undefined;
        item.iconPath = new vscode.ThemeIcon(ICONS[n.kind] ?? 'symbol-misc', n.inactive ? new vscode.ThemeColor('disabledForeground') : undefined);
        if (n.kind !== 'group') {
          const lines = [`**${n.name}** (${n.kind})`, n.detail ? `\`${n.detail}\`` : '', n.mark ?? '', n.inherited_from ? `Inherited from \`${n.inherited_from}\`` : ''];
          item.tooltip = new vscode.MarkdownString(lines.filter((l) => l).join('\n\n'));
        }
        const at = n.target ?? n.extent;
        if (at && m) {
          const pos = new vscode.Position(at.range.start.line, at.range.start.character);
          item.command = { command: 'vscode.open', title: 'Open', arguments: [m.toClient(at.uri), { selection: new vscode.Range(pos, pos) }] };
        }
        this.items.set(n.id, item);
        item.kids = make(n.children, item, depth + 1);
        return item;
      });
    this.roots = make(nodes, undefined, 0);
  }

  private present(): void {
    const r = this.result;
    const d = this.document();
    const file = d ? d.doc.uri.path.replace(/^.*\//, '') : undefined;
    const parts: string[] = [];
    if (file) {
      parts.push(file);
    }
    if (r?.status === 'not-compiled') {
      parts.push('not in the build');
    } else if (r?.context && r.context !== file) {
      parts.push(`via ${r.context}${r.others ? ` (+${r.others})` : ''}`);
    }
    if (this.filter) {
      parts.push(`filter '${this.filter}'`);
    }
    if (r?.incomplete) {
      parts.push('cut at the node limit');
    }
    this.view.description = parts.join(' · ') || undefined;
    if (!this.conn.client()) {
      this.view.message = 'The DVICode server is not running.';
    } else if (this.error) {
      this.view.message = `Could not get the structure: ${this.error}`;
    } else if (!d && !r) {
      this.view.message = 'Open a SystemVerilog file to see its structure.';
    } else if (r && !r.nodes.length) {
      this.view.message = this.filter ? `Nothing matches '${this.filter}'.` : r.status === 'none' ? 'DVICode has not read this file.' : 'No declarations.';
    } else {
      this.view.message = undefined;
    }
  }

  /** Selects the node at the cursor, without taking the focus. */
  private async follow(): Promise<void> {
    const ed = vscode.window.activeTextEditor;
    const d = this.document();
    if (!ed || !d || !this.result || d.uri !== this.uri || !this.view.visible) {
      return;
    }
    const pos = ed.selection.active;
    // Engine URIs are compared as VS Code shows them (drive letters, case, remote authorities).
    const m = this.conn.mapper();
    const doc = uriKey(ed.document.uri);
    const keys = new Map<string, boolean>();
    const inDoc = (uri: string): boolean => {
      let k = keys.get(uri);
      if (k === undefined) {
        k = m !== undefined && uriKey(m.toClient(uri)) === doc;
        keys.set(uri, k);
      }
      return k;
    };
    const chain = innermostAt(this.result.nodes, inDoc, pos.line, pos.character);
    const last = chain[chain.length - 1];
    const item = last && this.items.get(last.id);
    if (!item || this.view.selection[0] === item) {
      return;
    }
    try {
      await this.view.reveal(item, { select: true, focus: false, expand: false });
    } catch {
      // The view went away.
    }
  }
}
