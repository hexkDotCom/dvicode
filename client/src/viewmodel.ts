// What the DVICode views show, without VS Code: item keys, the occurrences of the editor's
// position in a tree, "k of N" cycling, and the texts of a view's description and message.

/** A node of any of the three trees. */
export interface TreeNode {
  children: TreeNode[];
  truncated?: boolean;
}

/** A node of the tree a view shows, with its place in it. */
export interface Placed<N extends TreeNode> {
  node: N;
  parent: Placed<N> | undefined;
  children: Placed<N>[];
  /** Names from the root, as the engine names nodes (`reveal`, `dvicode/tree`). */
  names: string[];
  /** Unique in the tree: names, with `#2`, `#3`... on repeated sibling names. */
  key: string;
}

export interface PlacedTree<N extends TreeNode> {
  roots: Placed<N>[];
  byKey: Map<string, Placed<N>>;
  /** By the key of `names` (several nodes when siblings share a name). */
  byNames: Map<string, Placed<N>[]>;
  /** Some node has children that were not returned. */
  truncated: boolean;
}

export function namesKey(names: string[]): string {
  return JSON.stringify(names);
}

/** Places the nodes of a tree; `nameOf` names a node (`root`: a node at the top). */
export function placeTree<N extends TreeNode>(roots: N[], nameOf: (n: N, root: boolean) => string): PlacedTree<N> {
  const tree: PlacedTree<N> = { roots: [], byKey: new Map(), byNames: new Map(), truncated: false };
  const place = (nodes: N[], parent: Placed<N> | undefined): Placed<N>[] => {
    const seen = new Map<string, number>();
    return nodes.map((n) => {
      const name = nameOf(n, parent === undefined);
      const k = (seen.get(name) ?? 0) + 1;
      seen.set(name, k);
      const names = parent ? [...parent.names, name] : [name];
      const segment = k > 1 ? `${name}#${k}` : name;
      const key = parent ? `${parent.key}\u0000${segment}` : segment;
      const p: Placed<N> = { node: n, parent, children: [], names, key };
      tree.byKey.set(key, p);
      const nk = namesKey(names);
      tree.byNames.set(nk, [...(tree.byNames.get(nk) ?? []), p]);
      tree.truncated ||= n.truncated === true;
      p.children = place(n.children as N[], p);
      return p;
    });
  };
  tree.roots = place(roots, undefined);
  return tree;
}

/** Nodes of a tree in the order the view shows them (depth first). */
export function depthFirst<N extends TreeNode>(roots: Placed<N>[]): Placed<N>[] {
  const out: Placed<N>[] = [];
  const walk = (ps: Placed<N>[]): void => {
    for (const p of ps) {
      out.push(p);
      walk(p.children);
    }
  };
  walk(roots);
  return out;
}

/** The editor's position as nodes of a view (from `dvicode/locate`). */
export interface Target {
  /** Design Hierarchy: instances of this module (or, with `instance`, that instance in it). */
  definition?: string;
  instance?: string;
  /** Verification Hierarchy: components of this class. */
  class?: string;
  /** Compiled Files: this file (a client URI string). */
  file?: string;
}

export function targetKey(t: Target): string {
  return JSON.stringify([t.definition ?? null, t.instance ?? null, t.class ?? null, t.file ?? null]);
}

export function describeTarget(t: Target): string {
  if (t.class) {
    return `class '${t.class}'`;
  }
  if (t.definition && t.instance) {
    return `instance '${t.instance}' of '${t.definition}'`;
  }
  if (t.definition) {
    return `'${t.definition}'`;
  }
  return t.file ? `'${t.file.replace(/^.*\//, '')}'` : 'this position';
}

interface DesignLike extends TreeNode {
  name: string;
  definition: string;
  kind: string;
}

/** Whether a Design Hierarchy node is the target: an instance of the module, or the instance in it. */
export function isDesignTarget(p: Placed<DesignLike>, t: Target): boolean {
  if (!t.definition) {
    return false;
  }
  if (!t.instance) {
    return p.node.definition === t.definition;
  }
  if (p.node.name !== t.instance || !p.parent) {
    return false;
  }
  // The design element it is written in: the nearest parent that is not a generate block.
  let up: Placed<DesignLike> | undefined = p.parent;
  while (up && up.node.kind === 'generate') {
    up = up.parent;
  }
  return up?.node.definition === t.definition;
}

/** Whether a Verification Hierarchy node is a component of the target class. */
export function isClassTarget(p: Placed<TreeNode & { class: string }>, t: Target): boolean {
  return t.class !== undefined && p.node.class === t.class;
}

/** The names of the nodes that are hits, in view order. */
export function occurrences<N extends TreeNode>(tree: PlacedTree<N>, hit: (p: Placed<N>) => boolean): string[][] {
  return depthFirst(tree.roots)
    .filter(hit)
    .map((p) => p.names);
}

/**
 * Which occurrence of the editor's position a view shows: kept while the target stays the same
 * (moving inside a module does not go back to the first instance), else the one selected in
 * the view if it is an occurrence, else the first.
 */
export class Cycle {
  target: string | undefined;
  paths: string[][] = [];
  index = 0;

  /** Returns true when the occurrence to show changed. */
  update(target: string, paths: string[][], selected: string[] | undefined): boolean {
    const same = target === this.target;
    const before = this.current();
    this.target = target;
    this.paths = paths;
    const sel = selected ? paths.findIndex((p) => namesKey(p) === namesKey(selected)) : -1;
    if (sel >= 0) {
      this.index = sel;
    } else if (same && before) {
      const kept = paths.findIndex((p) => namesKey(p) === namesKey(before));
      this.index = kept >= 0 ? kept : Math.min(this.index, Math.max(paths.length - 1, 0));
    } else {
      this.index = 0;
    }
    const now = this.current();
    return !before || !now || namesKey(before) !== namesKey(now) || !same;
  }

  step(delta: number): string[] | undefined {
    if (!this.paths.length) {
      return undefined;
    }
    const n = this.paths.length;
    this.index = (((this.index + delta) % n) + n) % n;
    return this.current();
  }

  current(): string[] | undefined {
    return this.paths[this.index];
  }

  reset(): void {
    this.target = undefined;
    this.paths = [];
    this.index = 0;
  }
}

/** Why a tree is incomplete, for the description. */
export function incompleteText(incomplete: string | null | undefined, matches: number): string | undefined {
  switch (incomplete) {
    case 'matches':
      return `first ${matches.toLocaleString('en-US')} matches`;
    case 'visits':
      return 'search stopped early';
    case 'nodes':
      return 'cut at the node limit';
    default:
      return undefined;
  }
}

export interface ViewState {
  /** The engine is running. */
  connected: boolean;
  /** The last request failed. */
  error?: string;
  /** What the view is of: a top, a test. */
  scope?: string;
  filter?: string;
  matches?: number;
  incomplete?: string | null;
  /** The tree has no node. */
  empty: boolean;
  /** Why the tree is empty without a filter. */
  emptyText?: string;
  /** "k of N" while following the editor. */
  position?: { index: number; count: number };
  /** Why following the editor shows nothing. */
  syncNote?: string;
  /** The tree is older than the code (a large tree is not fetched again after each edit). */
  stale?: boolean;
}

/** The text after the view's title. */
export function viewDescription(s: ViewState): string | undefined {
  const parts: string[] = [];
  if (s.scope) {
    parts.push(s.scope);
  }
  if (s.filter) {
    const n = s.matches ?? 0;
    parts.push(`filter '${s.filter}': ${n.toLocaleString('en-US')} ${n === 1 ? 'match' : 'matches'}`);
  }
  const inc = incompleteText(s.incomplete, s.matches ?? 0);
  if (inc && (s.filter || s.incomplete !== 'nodes')) {
    parts.push(inc);
  }
  if (s.stale) {
    parts.push('out of date: Refresh');
  }
  if (s.position && s.position.count > 1) {
    parts.push(`${s.position.index + 1} of ${s.position.count}`);
  }
  return parts.length ? parts.join(' · ') : undefined;
}

/** The message above the tree: the first that applies, in this order. */
export function viewMessage(s: ViewState): string | undefined {
  if (!s.connected) {
    return 'The DVICode server is not running.';
  }
  if (s.error) {
    return `Could not get the tree: ${s.error}`;
  }
  if (s.filter && s.empty) {
    return `Nothing matches '${s.filter}'.`;
  }
  if (s.empty && s.emptyText) {
    return s.emptyText;
  }
  return s.syncNote;
}
