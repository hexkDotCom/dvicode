// Tree views: design hierarchy, UVM verification hierarchy and compiled files. Each view can be
// filtered and can follow the editor (see viewmodel.ts for the rules).

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import type { Mapper } from './pathmap';
import {
  Cycle,
  Placed,
  PlacedTree,
  Target,
  TreeNode,
  ViewState,
  describeTarget,
  isClassTarget,
  isDesignTarget,
  namesKey,
  occurrences,
  placeTree,
  targetKey,
  viewDescription,
  viewMessage,
} from './viewmodel';

export interface Conn {
  client(): LanguageClient | undefined;
  mapper(): Mapper | undefined;
}

interface HierNode extends TreeNode {
  name: string;
  /** Which of several siblings of this name (2, 3...). */
  dup?: number;
  definition: string;
  kind: string;
  uri: string | null;
  line: number;
  character: number;
  def_uri: string | null;
  def_line: number;
  children: HierNode[];
  truncated: boolean;
}

interface FileNode extends TreeNode {
  path: string;
  truncated?: boolean;
  /** The same file at the top more than once: which one (2, 3...). */
  dup?: number;
  uri: string;
  kind: string;
  children: FileNode[];
}

interface VerifNode extends TreeNode {
  name: string;
  dup?: number;
  path: string;
  class: string;
  kind: string;
  uri: string | null;
  line: number;
  character: number;
  def_uri: string | null;
  def_line: number;
  children: VerifNode[];
  truncated: boolean;
}

export interface VerifHierarchyResult {
  test: string | null;
  configured: string | null;
  tests: string[];
  roots: VerifNode[];
  /** Without tests: why there may be none. */
  hint?: string;
}

/** `dvicode/tree`. */
interface TreeResult<N> {
  revision: number;
  incomplete: string | null;
  matches: number;
  roots: N[];
  test?: string | null;
  configured?: string | null;
  tests?: string[];
  hint?: string;
}

/** `dvicode/locate`. */
export interface Located {
  revision: number;
  design: { name: string; uri: string | null } | null;
  instance: string | null;
  class: { name: string; uri: string | null } | null;
}

export type ViewName = 'design' | 'verification' | 'files';

/** A node of a view. */
export class Item<N extends TreeNode> extends vscode.TreeItem {
  kids: Item<N>[] = [];
  constructor(
    readonly placed: Placed<N>,
    readonly parent: Item<N> | undefined,
    label: string | vscode.Uri,
    state: vscode.TreeItemCollapsibleState,
  ) {
    super(label as string, state);
  }

  get node(): N {
    return this.placed.node;
  }
}

/** Whether a follow started earlier is still the one to carry out. */
export type StillWanted = () => boolean;

/** A name in paths: `f`, or `f#2` for the second sibling called `f` (as the engine names it). */
function keyedName(name: string, dup: number | undefined): string {
  return dup ? `${name}#${dup}` : name;
}

/** What a view remembers per build configuration (workspaceState). */
interface Remembered {
  filter?: string;
  sync?: boolean;
}

/** The shared part of the three views. */
export abstract class TreeBase<N extends TreeNode> implements vscode.TreeDataProvider<Item<N>> {
  private readonly emitter = new vscode.EventEmitter<Item<N> | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  view: vscode.TreeView<Item<N>> | undefined;
  filter: string | undefined;
  sync = false;
  /** Changes with each filter, so items get new ids and the matches show expanded. */
  private filterGen = 0;
  private tree: PlacedTree<N> | undefined;
  private items = new Map<string, Item<N>>();
  private roots: Item<N>[] = [];
  private revision = -1;
  private error: string | undefined;
  private incomplete: string | null = null;
  private matches = 0;
  /** Request generation: an answer to an older request is dropped. */
  private gen = 0;
  private loading: Promise<void> | undefined;
  /** A fetch was started since the last change of parameters (getChildren fetches only once). */
  private attempted = false;
  /** The item the view selected itself, following the editor. */
  private revealed: Item<N> | undefined;
  /** Shown tree is older than the analysis (a visible view refreshes at once). */
  stale = false;
  syncNote: string | undefined;
  readonly cycle = new Cycle();
  /** When the user last selected a node in the view (not when following the editor did). */
  lastPick = 0;
  revealing = false;
  /** Occurrences of the last target, for one revision of the tree. */
  private found: { key: string; scope: string; revision: number; paths: string[][]; more: boolean } | undefined;
  /** The engine stopped before finding every occurrence. */
  private moreOccurrences = false;
  /** The newest analysis revision known (the tree shown may be older, see `stale`). */
  latest = -1;
  /** Changes when the top, test, filter or server changes: older follows stop. */
  private paramGen = 0;

  constructor(
    readonly name: ViewName,
    readonly viewId: string,
    protected readonly conn: Conn,
  ) {}

  /** Request parameters besides the filter (top, test). */
  protected abstract params(): Record<string, unknown>;
  /** The name of a node in paths. */
  protected abstract nameOf(n: N, root: boolean): string;
  protected abstract makeItem(p: Placed<N>, parent: Item<N> | undefined, expand: boolean): Item<N>;
  /** The description's first part (the top, the test). */
  protected abstract scopeText(): string | undefined;
  /** Why the tree is empty without a filter. */
  protected abstract emptyText(): string | undefined;
  /** Takes what else the answer holds (the tests). */
  protected onResult(r: TreeResult<N>): void {
    void r;
  }

  attach(view: vscode.TreeView<Item<N>>): void {
    this.view = view;
    view.onDidChangeSelection((e) => {
      if (!this.revealing && e.selection[0] !== this.revealed) {
        this.lastPick = Date.now();
      }
    });
    view.onDidChangeVisibility((e) => {
      if (e.visible && this.stale) {
        void this.refresh();
      }
    });
  }

  getTreeItem(e: Item<N>): vscode.TreeItem {
    return e;
  }

  getParent(e: Item<N>): Item<N> | undefined {
    return e.parent;
  }

  async getChildren(e?: Item<N>): Promise<Item<N>[]> {
    if (e) {
      return e.kids;
    }
    if (!this.attempted) {
      await (this.loading ?? this.refresh());
    } else if (this.loading) {
      await this.loading;
    }
    return this.roots;
  }

  /** Fetches the tree again (`reveal`: names of a node to include beyond the node limit). */
  refresh(reveal?: string[], wanted?: StillWanted): Promise<void> {
    const p = this.load(reveal ?? (this.sync && !this.filter ? this.cycle.current() : undefined), wanted);
    this.loading = p;
    void p.finally(() => {
      if (this.loading === p) {
        this.loading = undefined;
      }
    });
    return p;
  }

  /** Forgets the occurrences found (a restarted server, a refresh): they are searched again. */
  forget(): void {
    this.found = undefined;
    this.moreOccurrences = false;
  }

  /** Forgets also the occurrence shown, and stops follows under way (new parameters, server). */
  reset(): void {
    this.forget();
    this.latest = -1;
    // Answers to requests made before are dropped.
    this.gen++;
    this.cycle.reset();
    this.paramGen++;
  }

  /** Forgets the tree (its parameters changed): the next getChildren fetches it. */
  protected invalidate(): void {
    this.reset();
    this.gen++;
    this.attempted = false;
    this.tree = undefined;
    this.error = undefined;
    this.roots = [];
    this.items.clear();
  }

  private async load(reveal?: string[], wanted?: StillWanted): Promise<void> {
    const gen = ++this.gen;
    this.attempted = true;
    const c = this.conn.client();
    if (!c) {
      this.tree = undefined;
      this.roots = [];
      this.present();
      this.emitter.fire(undefined);
      return;
    }
    try {
      const r = await c.sendRequest<TreeResult<N>>('dvicode/tree', {
        view: this.name,
        ...this.params(),
        filter: this.filter ?? null,
        reveal: reveal ?? null,
      });
      if (gen !== this.gen || (wanted && !wanted())) {
        return;
      }
      this.onResult(r);
      this.revision = r.revision;
      this.latest = Math.max(this.latest, r.revision);
      this.incomplete = r.incomplete;
      this.matches = r.matches;
      this.error = undefined;
      this.stale = r.revision < this.latest;
      this.setTree(r.roots);
    } catch (e) {
      if (gen !== this.gen) {
        return;
      }
      this.error = e instanceof Error ? e.message : String(e);
      this.tree = undefined;
      this.roots = [];
      this.items.clear();
    }
    this.present();
    this.emitter.fire(undefined);
  }

  private setTree(roots: N[]): void {
    const tree = placeTree(roots, (n, root) => this.nameOf(n, root));
    this.tree = tree;
    this.items.clear();
    const prefix = this.filter ? `f${this.filterGen}:` : '';
    const make = (ps: Placed<N>[], parent: Item<N> | undefined): Item<N>[] =>
      ps.map((p) => {
        // With a filter the parents of matches are open.
        const item = this.makeItem(p, parent, this.filter !== undefined || (parent === undefined && this.expandRoots(tree)));
        item.id = `${this.name}:${prefix}${p.key}`;
        this.items.set(p.key, item);
        item.kids = make(p.children, item);
        return item;
      });
    this.roots = make(tree.roots, undefined);
  }

  /** Whether root nodes show open. */
  protected expandRoots(tree: PlacedTree<N>): boolean {
    void tree;
    return true;
  }

  /** Sets the view's description and message. */
  present(): void {
    if (!this.view) {
      return;
    }
    const s: ViewState = {
      connected: this.conn.client() !== undefined,
      error: this.error,
      scope: this.scopeText(),
      filter: this.filter,
      matches: this.matches,
      incomplete: this.incomplete,
      empty: this.roots.length === 0,
      emptyText: this.emptyText(),
      position: this.sync && this.cycle.paths.length ? { index: this.cycle.index, count: this.cycle.paths.length, more: this.moreOccurrences } : undefined,
      syncNote: this.sync ? this.syncNote : undefined,
      stale: this.stale && this.items.size > LARGE_TREE,
    };
    this.view.description = viewDescription(s);
    this.view.message = viewMessage(s);
  }

  async setFilter(text: string | undefined): Promise<void> {
    const t = text?.trim() || undefined;
    if (t === this.filter) {
      return;
    }
    this.filter = t;
    this.filterGen++;
    this.paramGen++;
    this.syncNote = undefined;
    await this.refresh();
  }

  // ---------------------------------------------------------------- following the editor

  /** The occurrences of `t` in the whole tree (not only the nodes shown), in view order. */
  protected async occurrencesOf(t: Target): Promise<string[][]> {
    const key = targetKey(t);
    const scope = JSON.stringify(this.params());
    if (this.found && this.found.key === key && this.found.scope === scope && this.found.revision === this.latest) {
      this.moreOccurrences = this.found.more;
      return this.found.paths;
    }
    const c = this.conn.client();
    if (!c) {
      return [];
    }
    const target = { definition: t.definition ?? null, instance: t.instance ?? null, class: t.class ?? null };
    const pg = this.paramGen;
    const r = await c.sendRequest<TreeResult<N>>('dvicode/tree', { view: this.name, ...this.params(), target, maxNodes: 100000 });
    if (pg !== this.paramGen) {
      // The top, test, filter or server changed meanwhile.
      return [];
    }
    const tree = placeTree(r.roots, (n, root) => this.nameOf(n, root));
    const paths = occurrences(tree, (p) => this.isTarget(p, t));
    this.latest = Math.max(this.latest, r.revision);
    this.found = { key, scope, revision: r.revision, paths, more: r.incomplete !== null };
    this.moreOccurrences = r.incomplete !== null;
    return paths;
  }

  protected abstract isTarget(p: Placed<N>, t: Target): boolean;

  /** Why the target has no node here. */
  protected abstract notFoundText(t: Target): string;

  /** Shows the node of the editor's position (`t`: undefined when there is none). */
  async follow(t: Target | undefined, stillWanted: StillWanted = () => true): Promise<void> {
    const pg = this.paramGen;
    const wanted: StillWanted = () => stillWanted() && pg === this.paramGen;
    if (!t) {
      this.cycle.reset();
      this.syncNote = this.name === 'verification' ? 'The cursor is not in a class.' : this.name === 'design' ? 'The cursor is not in a module.' : undefined;
      this.present();
      return;
    }
    const paths = t.file ? this.fileOccurrences(t.file) : await this.occurrencesOf(t);
    if (!wanted()) {
      return;
    }
    if (t.file) {
      this.moreOccurrences = this.incomplete !== null;
    }
    if (!paths.length) {
      this.cycle.reset();
      this.syncNote = t.file && this.incomplete !== null ? `${describeTarget(t)} is not in the part of the tree shown (node limit).` : this.notFoundText(t);
      this.present();
      return;
    }
    const selected = this.view?.selection[0]?.placed.names;
    this.cycle.update(targetKey(t), paths, selected);
    await this.showCurrent(t, wanted);
  }

  /** Files view: the nodes of a file (a client URI string). */
  protected fileOccurrences(file: string): string[][] {
    void file;
    return [];
  }

  async step(delta: number): Promise<void> {
    if (this.cycle.step(delta)) {
      await this.showCurrent(undefined);
    }
  }

  private async showCurrent(t: Target | undefined, wanted: StillWanted = () => true): Promise<void> {
    const names = this.cycle.current();
    if (!names) {
      return;
    }
    let item = this.itemByNames(names);
    if (!item) {
      if (this.filter) {
        this.syncNote = `${t ? describeTarget(t) : 'The node'} is hidden by the filter.`;
        this.present();
        return;
      }
      // Beyond the node limit: fetch the tree with this node in it.
      await this.refresh(names, wanted);
      if (!wanted()) {
        return;
      }
      item = this.itemByNames(names);
      if (!item) {
        this.syncNote = `${t ? describeTarget(t) : 'The node'} is not in the view.`;
        this.present();
        return;
      }
    }
    this.syncNote = undefined;
    this.present();
    if (!this.view || !this.view.visible || !wanted()) {
      return;
    }
    this.revealing = true;
    this.revealed = item;
    try {
      await this.view.reveal(item, { select: true, focus: false, expand: false });
    } catch {
      // The tree changed while revealing.
    } finally {
      this.revealing = false;
    }
  }

  private itemByNames(names: string[]): Item<N> | undefined {
    const placed = this.tree?.byNames.get(namesKey(names))?.[0];
    return placed ? this.items.get(placed.key) : undefined;
  }

  /** Placed nodes of the tree shown, depth first. */
  protected placed(): PlacedTree<N> | undefined {
    return this.tree;
  }

  /** Nodes in the tree shown. */
  get size(): number {
    return this.items.size;
  }
}

/** Trees larger than this are not fetched again after each edit (the view says it is out of date). */
const LARGE_TREE = 5000;

function openCommand(conn: Conn, uri: string | null, line: number, character: number): vscode.Command | undefined {
  const m = conn.mapper();
  if (!uri || !m) {
    return undefined;
  }
  const pos = new vscode.Position(line, character);
  return { command: 'vscode.open', title: 'Open', arguments: [m.toClient(uri), { selection: new vscode.Range(pos, pos) }] };
}

/**
 * A key to compare document URIs: VS Code may give the same document's authority in another case
 * (`dvicode://wsl+ubuntu/...` for `wsl+Ubuntu`), and Windows paths ignore case.
 */
export function uriKey(u: vscode.Uri): string {
  const key = `${u.scheme}://${u.authority.toLowerCase()}${u.path}`;
  // Windows paths ignore case (drives and network shares); a WSL folder's Linux path does not.
  const wsl = /^wsl(\.localhost|\$)$/i.test(u.authority);
  return u.scheme === 'file' && !wsl && process.platform === 'win32' ? key.toLowerCase() : key;
}

/** The key of an engine URI as VS Code shows it, to compare with documents. */
function clientUri(conn: Conn, uri: string | null): string | undefined {
  const m = conn.mapper();
  return uri && m ? uriKey(m.toClient(uri)) : undefined;
}

const DESIGN_ICONS: Record<string, string> = { generate: 'symbol-namespace', interface: 'symbol-interface', unresolved: 'warning' };

export class DesignHierarchy extends TreeBase<HierNode> {
  private topValue: string | undefined;

  constructor(conn: Conn) {
    super('design', 'dvicode.designHierarchy', conn);
  }

  get top(): string | undefined {
    return this.topValue;
  }

  set top(t: string | undefined) {
    this.topValue = t;
    this.invalidate();
  }

  protected params(): Record<string, unknown> {
    return { top: this.topValue ?? null };
  }

  protected nameOf(n: HierNode): string {
    return keyedName(n.name, n.dup);
  }

  protected makeItem(p: Placed<HierNode>, parent: Item<HierNode> | undefined, expand: boolean): Item<HierNode> {
    const n = p.node;
    const state = n.children.length ? (expand ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed) : vscode.TreeItemCollapsibleState.None;
    const item = new Item(p, parent, n.name, state);
    item.description = (n.definition && n.definition !== n.name ? n.definition : '') + (n.truncated ? ' …' : '') || undefined;
    item.contextValue = 'dvicode.hierNode';
    item.iconPath = new vscode.ThemeIcon(DESIGN_ICONS[n.kind] ?? 'symbol-module');
    item.command = openCommand(this.conn, n.uri, n.line, n.character);
    if (item.command) {
      item.tooltip = new vscode.MarkdownString(`**${n.name}** : \`${n.definition || n.kind}\`\n\n${p.names.join('.')}`);
    }
    return item;
  }

  protected scopeText(): string | undefined {
    return this.topValue ? `top ${this.topValue}` : undefined;
  }

  protected emptyText(): string | undefined {
    return this.topValue ? `No design element '${this.topValue}'.` : 'No design element in the project.';
  }

  protected isTarget(p: Placed<HierNode>, t: Target): boolean {
    return isDesignTarget(p, t);
  }

  protected notFoundText(t: Target): string {
    return `${describeTarget(t)} is not instantiated under ${this.topValue ? `'${this.topValue}'` : 'the tops shown'}.`;
  }
}

export class CompiledFiles extends TreeBase<FileNode> {
  constructor(conn: Conn) {
    super('files', 'dvicode.compiledFiles', conn);
  }

  protected params(): Record<string, unknown> {
    return {};
  }

  protected nameOf(n: FileNode): string {
    return keyedName(n.uri, n.dup);
  }

  protected makeItem(p: Placed<FileNode>, parent: Item<FileNode> | undefined, expand: boolean): Item<FileNode> {
    const n = p.node;
    const uri = this.conn.mapper()?.toClient(n.uri) ?? vscode.Uri.parse(n.uri);
    const state = n.children.length ? (expand ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed) : vscode.TreeItemCollapsibleState.None;
    const item = new Item(p, parent, uri, state);
    item.resourceUri = uri;
    item.description = n.kind === 'top' ? undefined : n.kind;
    if (n.truncated) {
      item.description = `${item.description ?? ''} …`.trim();
    }
    item.tooltip = n.path;
    item.command = { command: 'vscode.open', title: 'Open', arguments: [uri] };
    return item;
  }

  protected expandRoots(): boolean {
    return false;
  }

  protected scopeText(): string | undefined {
    return undefined;
  }

  protected emptyText(): string | undefined {
    return 'No file compiled.';
  }

  protected isTarget(): boolean {
    return false;
  }

  protected notFoundText(t: Target): string {
    return `${describeTarget(t)} is not compiled.`;
  }

  protected fileOccurrences(file: string): string[][] {
    const tree = this.placed();
    if (!tree) {
      return [];
    }
    return occurrences(tree, (p) => clientUri(this.conn, p.node.uri) === file);
  }
}

export function hierarchyPath(item: unknown): string | undefined {
  if (!(item instanceof Item)) {
    return undefined;
  }
  const n = item.node as Partial<VerifNode>;
  if (typeof n.path === 'string' && typeof n.class === 'string') {
    return n.path;
  }
  if (!('definition' in item.node)) {
    return undefined;
  }
  const names: string[] = [];
  for (let p: Placed<TreeNode> | undefined = item.placed; p; p = p.parent) {
    names.unshift((p.node as unknown as HierNode).name);
  }
  return names.join('.');
}

const VERIF_ICONS: Record<string, string> = {
  test: 'beaker',
  env: 'symbol-namespace',
  agent: 'person',
  driver: 'arrow-right',
  monitor: 'eye',
  sequencer: 'list-ordered',
  scoreboard: 'checklist',
  subscriber: 'rss',
  port: 'plug',
  component: 'symbol-class',
  unresolved: 'warning',
};

/** UVM component tree of the selected test (`+UVM_TESTNAME` by default). */
export class VerificationHierarchy extends TreeBase<VerifNode> {
  private testValue: string | undefined;
  last: VerifHierarchyResult | undefined;

  constructor(conn: Conn) {
    super('verification', 'dvicode.verificationHierarchy', conn);
  }

  /** Test class to show, `*` for all tests, undefined for the configured one. */
  get test(): string | undefined {
    return this.testValue;
  }

  set test(t: string | undefined) {
    this.testValue = t;
    this.invalidate();
  }

  protected params(): Record<string, unknown> {
    return { test: this.testValue ?? null };
  }

  protected onResult(r: TreeResult<VerifNode>): void {
    this.last = { test: r.test ?? null, configured: r.configured ?? null, tests: r.tests ?? [], roots: r.roots, hint: r.hint };
  }

  /** The test class names the root; components their instance names. */
  protected nameOf(n: VerifNode, root: boolean): string {
    return keyedName(root ? n.class : n.name, n.dup);
  }

  protected expandRoots(tree: PlacedTree<VerifNode>): boolean {
    return tree.roots.length === 1;
  }

  protected makeItem(p: Placed<VerifNode>, parent: Item<VerifNode> | undefined, expand: boolean): Item<VerifNode> {
    const n = p.node;
    const state = n.children.length ? (expand ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed) : vscode.TreeItemCollapsibleState.None;
    const item = new Item(p, parent, n.name, state);
    item.description = n.class + (n.truncated ? ' …' : '');
    item.contextValue = n.def_uri ? 'dvicode.verifNode.class' : 'dvicode.verifNode';
    item.iconPath = new vscode.ThemeIcon(VERIF_ICONS[n.kind] ?? 'symbol-class');
    item.tooltip = new vscode.MarkdownString(`**${n.name}** : \`${n.class}\` (${n.kind})\n\n${n.path}`);
    item.command = openCommand(this.conn, n.uri, n.line, n.character);
    return item;
  }

  protected scopeText(): string | undefined {
    const r = this.last;
    if (!r) {
      return undefined;
    }
    return r.test ?? (r.roots.length ? 'all tests' : undefined);
  }

  protected emptyText(): string | undefined {
    const r = this.last;
    if (!r) {
      return undefined;
    }
    if (r.tests.length) {
      return `Test '${r.test}' not found. Select one of the ${r.tests.length} tests.`;
    }
    return r.hint ?? 'No UVM test (class derived from uvm_test) in the project.';
  }

  protected isTarget(p: Placed<VerifNode>, t: Target): boolean {
    return isClassTarget(p, t);
  }

  protected notFoundText(t: Target): string {
    return `${describeTarget(t)} is not a component of ${this.last?.test ? `test '${this.last.test}'` : 'the tests shown'}.`;
  }
}

export function componentClassLocation(item: unknown, conn: Conn): { uri: vscode.Uri; line: number } | undefined {
  if (!(item instanceof Item)) {
    return undefined;
  }
  const n = item.node as Partial<VerifNode>;
  const m = conn.mapper();
  return n.def_uri && typeof n.class === 'string' && m ? { uri: m.toClient(n.def_uri), line: n.def_line ?? 0 } : undefined;
}

/** The three views, their commands, what they remember, and following the editor. */
export class Views implements vscode.Disposable {
  readonly design: DesignHierarchy;
  readonly verification: VerificationHierarchy;
  readonly files: CompiledFiles;
  private readonly all: TreeBase<TreeNode>[];
  private readonly subs: vscode.Disposable[] = [];
  private timer: NodeJS.Timeout | undefined;
  private revisionTimer: NodeJS.Timeout | undefined;
  /** The last located position and what the engine said. */
  private located: { key: string; at: Promise<Located | undefined> } | undefined;
  /** Changes with each editor move, sync switch and pick in a view: older follows stop. */
  private followGen = 0;

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly conn: Conn,
  ) {
    this.design = new DesignHierarchy(conn);
    this.verification = new VerificationHierarchy(conn);
    this.files = new CompiledFiles(conn);
    this.all = [this.design, this.verification, this.files] as unknown as TreeBase<TreeNode>[];
    for (const v of this.all) {
      const view = vscode.window.createTreeView(v.viewId, { treeDataProvider: v, showCollapseAll: v.name !== 'files' });
      v.attach(view);
      this.subs.push(
        view,
        view.onDidChangeVisibility((e) => {
          if (e.visible && v.sync) {
            this.schedule(0);
          }
        }),
      );
      this.registerCommands(v);
    }
    this.restore();
    this.subs.push(
      vscode.window.onDidChangeActiveTextEditor(() => this.schedule(100)),
      vscode.window.onDidChangeTextEditorSelection((e) => {
        if (e.textEditor === vscode.window.activeTextEditor) {
          this.schedule(300);
        }
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('dvicode.buildFile')) {
          this.restore();
        }
      }),
    );
  }

  dispose(): void {
    clearTimeout(this.timer);
    clearTimeout(this.revisionTimer);
    for (const s of this.subs) {
      s.dispose();
    }
  }

  /** After a full build: every view again. */
  refreshAll(): void {
    this.located = undefined;
    for (const v of this.all) {
      v.forget();
      void v.refresh().then(() => {
        if (v.sync) {
          this.schedule(0);
        }
      });
    }
  }

  /** The analysis changed after edits: visible views refresh (once the typing pauses). */
  onRevision(revision?: number): void {
    this.located = undefined;
    for (const v of this.all) {
      v.stale = true;
      if (revision !== undefined) {
        v.latest = Math.max(v.latest, revision);
      }
    }
    clearTimeout(this.revisionTimer);
    this.revisionTimer = setTimeout(() => {
      for (const v of this.all) {
        if (v.view?.visible && v.size <= LARGE_TREE) {
          void v.refresh().then(() => {
            if (v.sync) {
              this.schedule(0);
            }
          });
        } else {
          v.present();
        }
      }
    }, 1500);
  }

  /** The server stopped or started. */
  presentAll(): void {
    for (const v of this.all) {
      v.reset();
      v.present();
    }
  }

  private registerCommands(v: TreeBase<TreeNode>): void {
    const id = (c: string): string => `dvicode.${v.name}.${c}`;
    this.subs.push(
      vscode.commands.registerCommand(id('filter'), async () => {
        const text = await vscode.window.showInputBox({
          title: `Filter ${v.view?.title ?? v.name}`,
          value: v.filter ?? '',
          prompt: 'Text anywhere in a name or path; * within one level, ** across levels, ? one character. Empty: no filter.',
        });
        if (text !== undefined) {
          await v.setFilter(text);
          this.save(v);
          this.followAgain(v);
        }
      }),
      vscode.commands.registerCommand(id('clearFilter'), async () => {
        await v.setFilter(undefined);
        this.save(v);
        this.followAgain(v);
      }),
      vscode.commands.registerCommand(id('syncOn'), () => this.setSync(v, true)),
      vscode.commands.registerCommand(id('syncOff'), () => this.setSync(v, false)),
      vscode.commands.registerCommand(id('nextOccurrence'), () => v.step(1)),
      vscode.commands.registerCommand(id('previousOccurrence'), () => v.step(-1)),
    );
  }

  /** After the tree of a view that follows the editor changed (filter, top, test). */
  followAgain(v: TreeBase<TreeNode>): void {
    if (v.sync) {
      this.schedule(0);
    }
  }

  private setSync(v: TreeBase<TreeNode>, on: boolean): void {
    this.followGen++;
    v.sync = on;
    v.syncNote = undefined;
    v.cycle.reset();
    this.save(v);
    v.present();
    if (on) {
      this.schedule(0);
    }
  }

  /** Key of what the views remember: the build configuration. */
  private stateKey(v: TreeBase<TreeNode>): string {
    const build = vscode.workspace.getConfiguration('dvicode').get<string>('buildFile', '') || 'default';
    return `dvicode.view.${v.name}:${build}`;
  }

  private save(v: TreeBase<TreeNode>): void {
    const r: Remembered = { filter: v.filter, sync: v.sync };
    void this.ctx.workspaceState.update(this.stateKey(v), r);
    this.setContext(v);
  }

  private restore(): void {
    for (const v of this.all) {
      const r = this.ctx.workspaceState.get<Remembered>(this.stateKey(v)) ?? {};
      const filter = r.filter || undefined;
      v.sync = r.sync === true;
      v.cycle.reset();
      if (filter !== v.filter) {
        void v.setFilter(filter);
      }
      this.setContext(v);
      v.present();
    }
    this.schedule(0);
  }

  private setContext(v: TreeBase<TreeNode>): void {
    void vscode.commands.executeCommand('setContext', `dvicode.${v.name}.filtered`, v.filter !== undefined);
    void vscode.commands.executeCommand('setContext', `dvicode.${v.name}.sync`, v.sync);
  }

  private schedule(ms: number): void {
    this.followGen++;
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.follow(), ms);
  }

  /** Each visible view that follows the editor shows the editor's position. */
  private async follow(): Promise<void> {
    const token = this.followGen;
    const started = Date.now();
    const views = this.all.filter((v) => v.sync && v.view?.visible && started - v.lastPick > 1000);
    // Still wanted: no newer follow, still following, and no pick in the view since.
    const wanted = (v: TreeBase<TreeNode>): StillWanted => () => token === this.followGen && v.sync && v.lastPick < started;
    const ed = vscode.window.activeTextEditor;
    const m = this.conn.mapper();
    if (!views.length || !ed || !m || !this.ours(ed.document.uri)) {
      return;
    }
    const doc = uriKey(ed.document.uri);
    const needLocate = views.some((v) => v.name !== 'files');
    const located = needLocate ? await this.locate(ed, m) : undefined;
    for (const v of views) {
      try {
        if (!wanted(v)()) {
          continue;
        }
        if (v.name === 'files') {
          await v.follow({ file: doc }, wanted(v));
        } else if (v.name === 'design') {
          await v.follow(located?.design ? { definition: located.design.name, instance: located.instance ?? undefined } : undefined, wanted(v));
        } else {
          await v.follow(located?.class ? { class: located.class.name } : undefined, wanted(v));
        }
      } catch {
        // The server went away; the views say so when they refresh.
      }
    }
  }

  /** Documents this window's engine knows: local files, or files of its own dvicode:// host. */
  private ours(uri: vscode.Uri): boolean {
    if (uri.scheme === 'file') {
      return true;
    }
    if (uri.scheme !== 'dvicode') {
      return false;
    }
    const m = this.conn.mapper();
    try {
      return m !== undefined && m.toClient(m.toServer(uri)).authority.toLowerCase() === uri.authority.toLowerCase();
    } catch {
      return false;
    }
  }

  private locate(ed: vscode.TextEditor, m: Mapper): Promise<Located | undefined> {
    const c = this.conn.client();
    if (!c) {
      return Promise.resolve(undefined);
    }
    const uri = m.toServer(ed.document.uri);
    const position = ed.selection.active;
    const key = `${uri}:${position.line}:${position.character}`;
    if (this.located?.key !== key) {
      const at = c.sendRequest<Located>('dvicode/locate', { textDocument: { uri }, position }).catch(() => undefined);
      this.located = { key, at };
    }
    return this.located.at;
  }
}
