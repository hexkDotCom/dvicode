// Tree views: design hierarchy, UVM verification hierarchy and compiled files.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import type { Mapper } from './pathmap';

export interface Conn {
  client(): LanguageClient | undefined;
  mapper(): Mapper | undefined;
}

interface HierNode {
  name: string;
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

interface FileNode {
  path: string;
  uri: string;
  kind: string;
  children: FileNode[];
}

class HierItem extends vscode.TreeItem {
  constructor(
    readonly node: HierNode,
    readonly parent: HierItem | undefined,
    conn: Conn,
  ) {
    super(
      node.name,
      node.children.length ? (parent ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.Expanded) : vscode.TreeItemCollapsibleState.None,
    );
    this.description = node.definition && node.definition !== node.name ? node.definition : undefined;
    this.contextValue = 'dvicode.hierNode';
    const icon =
      node.kind === 'generate'
        ? 'symbol-namespace'
        : node.kind === 'interface'
          ? 'symbol-interface'
          : node.kind === 'unresolved'
            ? 'warning'
            : 'symbol-module';
    this.iconPath = new vscode.ThemeIcon(icon);
    const m = conn.mapper();
    if (node.uri && m) {
      const uri = m.toClient(node.uri);
      const pos = new vscode.Position(node.line, node.character);
      this.command = { command: 'vscode.open', title: 'Open', arguments: [uri, { selection: new vscode.Range(pos, pos) }] };
      this.tooltip = new vscode.MarkdownString(`**${node.name}** : \`${node.definition || node.kind}\`\n\n${this.path()}`);
    }
  }

  path(): string {
    const parts: string[] = [];
    let cur: HierItem | undefined = this;
    while (cur) {
      parts.unshift(cur.node.name);
      cur = cur.parent;
    }
    return parts.join('.');
  }
}

export class DesignHierarchy implements vscode.TreeDataProvider<HierItem> {
  private readonly emitter = new vscode.EventEmitter<HierItem | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  top: string | undefined;

  constructor(private readonly conn: Conn) {}

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(e: HierItem): vscode.TreeItem {
    return e;
  }

  getParent(e: HierItem): HierItem | undefined {
    return e.parent;
  }

  async getChildren(e?: HierItem): Promise<HierItem[]> {
    if (e) {
      return e.node.children.map((c) => new HierItem(c, e, this.conn));
    }
    const c = this.conn.client();
    if (!c) {
      return [];
    }
    try {
      const nodes = await c.sendRequest<HierNode[]>('dvicode/designHierarchy', { top: this.top ?? null });
      return nodes.map((n) => new HierItem(n, undefined, this.conn));
    } catch {
      return [];
    }
  }
}

class FileItem extends vscode.TreeItem {
  constructor(readonly node: FileNode, conn: Conn) {
    super(
      conn.mapper()?.toClient(node.uri) ?? vscode.Uri.parse(node.uri),
      node.children.length ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None,
    );
    const uri = conn.mapper()?.toClient(node.uri) ?? vscode.Uri.parse(node.uri);
    this.description = node.kind === 'top' ? undefined : node.kind;
    this.tooltip = node.path;
    this.command = { command: 'vscode.open', title: 'Open', arguments: [uri] };
  }
}

export class CompiledFiles implements vscode.TreeDataProvider<FileItem> {
  private readonly emitter = new vscode.EventEmitter<FileItem | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  constructor(private readonly conn: Conn) {}

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(e: FileItem): vscode.TreeItem {
    return e;
  }

  async getChildren(e?: FileItem): Promise<FileItem[]> {
    if (e) {
      return e.node.children.map((c) => new FileItem(c, this.conn));
    }
    const c = this.conn.client();
    if (!c) {
      return [];
    }
    try {
      const nodes = await c.sendRequest<FileNode[]>('dvicode/compiledFiles');
      return nodes.map((n) => new FileItem(n, this.conn));
    } catch {
      return [];
    }
  }
}

export function hierarchyPath(item: unknown): string | undefined {
  if (item instanceof VerifItem) {
    return item.node.path;
  }
  return item instanceof HierItem ? item.path() : undefined;
}

interface VerifNode {
  name: string;
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

class VerifItem extends vscode.TreeItem {
  constructor(
    readonly node: VerifNode,
    expand: boolean,
    conn: Conn,
  ) {
    super(
      node.name,
      node.children.length ? (expand ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.Collapsed) : vscode.TreeItemCollapsibleState.None,
    );
    this.description = node.class + (node.truncated ? ' …' : '');
    this.contextValue = node.def_uri ? 'dvicode.verifNode.class' : 'dvicode.verifNode';
    this.iconPath = new vscode.ThemeIcon(VERIF_ICONS[node.kind] ?? 'symbol-class');
    this.tooltip = new vscode.MarkdownString(`**${node.name}** : \`${node.class}\` (${node.kind})\n\n${node.path}`);
    const m = conn.mapper();
    if (node.uri && m) {
      const uri = m.toClient(node.uri);
      const pos = new vscode.Position(node.line, node.character);
      this.command = { command: 'vscode.open', title: 'Open', arguments: [uri, { selection: new vscode.Range(pos, pos) }] };
    }
  }
}

/** UVM component tree of the selected test (`+UVM_TESTNAME` by default). */
export class VerificationHierarchy implements vscode.TreeDataProvider<VerifItem> {
  private readonly emitter = new vscode.EventEmitter<VerifItem | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  /** Test class to show, `*` for all tests, undefined for the configured one. */
  test: string | undefined;
  view: vscode.TreeView<VerifItem> | undefined;
  last: VerifHierarchyResult | undefined;

  constructor(private readonly conn: Conn) {}

  refresh(): void {
    this.emitter.fire(undefined);
  }

  getTreeItem(e: VerifItem): vscode.TreeItem {
    return e;
  }

  async getChildren(e?: VerifItem): Promise<VerifItem[]> {
    if (e) {
      return e.node.children.map((c) => new VerifItem(c, false, this.conn));
    }
    const c = this.conn.client();
    if (!c) {
      return [];
    }
    try {
      const r = await c.sendRequest<VerifHierarchyResult>('dvicode/verificationHierarchy', { test: this.test ?? null });
      this.last = r;
      if (this.view) {
        this.view.description = r.test ?? (r.roots.length ? 'all tests' : undefined);
        this.view.message = r.roots.length
          ? undefined
          : r.tests.length
            ? `Test '${r.test}' not found. Select one of the ${r.tests.length} tests.`
            : 'No UVM test (class derived from uvm_test) in the project.';
      }
      return r.roots.map((n) => new VerifItem(n, r.roots.length === 1, this.conn));
    } catch {
      return [];
    }
  }
}

export function componentClassLocation(item: unknown, conn: Conn): { uri: vscode.Uri; line: number } | undefined {
  if (!(item instanceof VerifItem) || !item.node.def_uri) {
    return undefined;
  }
  const m = conn.mapper();
  return m ? { uri: m.toClient(item.node.def_uri), line: item.node.def_line } : undefined;
}
