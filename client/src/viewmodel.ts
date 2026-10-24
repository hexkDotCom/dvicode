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
  /** The class's file (a server URI): another class of the same name elsewhere is not it. */
  classUri?: string;
  /** The class is a UVM object, not a component (shown only with objects on). */
  objectClass?: boolean;
  /** Compiled Files: this file (a client URI string). */
  file?: string;
}

export function targetKey(t: Target): string {
  return JSON.stringify([t.definition ?? null, t.instance ?? null, t.class ?? null, t.classUri ?? null, t.file ?? null]);
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

/** Whether a Verification Hierarchy node is a component of the target class (the class built,
 * or the one requested when an override built another), declared in the target's file. */
export function isClassTarget(p: Placed<TreeNode & { class: string; def_uri?: string | null; overrides?: OverrideStep[] }>, t: Target): boolean {
  if (t.class === undefined) {
    return false;
  }
  const n = p.node;
  if (n.class === t.class) {
    return !t.classUri || !n.def_uri || n.def_uri === t.classUri;
  }
  return (n.overrides ?? []).some((o) => o.applied && o.from === t.class);
}

/** One factory override of a Verification Hierarchy node's class. */
export interface OverrideStep {
  from: string;
  to: string;
  instance: boolean;
  path?: string;
  set_in: string;
  conditions?: string[][];
  applied: boolean;
}

/** What a Verification Hierarchy node says beyond its name (see the engine's `VerifNode`). */
export interface VerifInfo {
  name: string;
  path: string;
  class: string;
  kind: string;
  truncated?: boolean;
  conditions?: string[][];
  count?: string;
  loops?: string[];
  alternatives?: { class: string; conditions: string[][] }[];
  ports_hidden?: number;
  overrides?: OverrideStep[];
  object?: boolean;
  starts?: { on: string; path: boolean; phase: string; conditions?: string[][] }[];
  allocated?: string;
  notes?: string[];
}

/** Conditions as text: the ways, each its conditions joined by `&&`, joined by `or`. */
export function conditionsText(ways: string[][]): string {
  return ways.map((w) => w.map((c) => `\`${c}\``).join(' && ')).join(' or ');
}

/** A loop count for the description: `×4`, `×≤4` (up to), `×?`. */
function countText(count: string): string {
  return count.startsWith('up to ') ? `×≤${count.slice(6)}` : `×${count}`;
}

/** The text after a Verification Hierarchy node's name. */
export function verifDescription(n: VerifInfo): string {
  const parts = [n.class + (n.count ? ` ${countText(n.count)}` : '')];
  const overrides = n.overrides ?? [];
  if (overrides.some((o) => o.applied)) {
    parts[0] += ' (override)';
  }
  if (overrides.some((o) => !o.applied)) {
    parts.push('may be overridden');
  }
  if (n.conditions?.length) {
    parts.push('conditional');
  }
  if (n.alternatives?.length) {
    parts.push(`or ${n.alternatives.map((a) => a.class).join(', ')}`);
  }
  const ons = [...new Set((n.starts ?? []).map((s) => s.on))];
  if (ons.length === 1) {
    parts.push(`on ${ons[0].replace(/^uvm_test_top\./, '')}`);
  } else if (ons.length > 1) {
    parts.push(`on ${ons.length} sequencers`);
  } else if (n.kind === 'sequence') {
    parts.push('not started here');
  }
  return parts.join(' · ') + (n.truncated ? ' …' : '');
}

/** A Verification Hierarchy node's tooltip (Markdown). */
export function verifTooltip(n: VerifInfo): string {
  const lines = [`**${n.name}** : \`${n.class}\` (${n.kind})`, n.object ? '' : n.path];
  for (const o of n.overrides ?? []) {
    const what = `\`${o.from}\` → \`${o.to}\`: ${o.instance ? `instance override (${o.path})` : 'type override'} in \`${o.set_in}\``;
    lines.push(o.applied ? `Override: ${what}` : `May be overridden: ${what}${o.conditions?.length ? `, when ${conditionsText(o.conditions)}` : ''}`);
  }
  if (n.conditions?.length) {
    lines.push(n.conditions.some((w) => w.includes('?')) ? 'Created under conditions that are not known.' : `Created only when ${conditionsText(n.conditions)}.`);
  }
  for (const a of n.alternatives ?? []) {
    lines.push(`Or \`${a.class}\`${a.conditions.length ? ` when ${conditionsText(a.conditions)}` : ''}.`);
  }
  if (n.count) {
    const loops = (n.loops ?? []).map((l) => `\`${l}\``).join(', ');
    const times = n.count === '?' ? 'an unknown number of iterations' : n.count.startsWith('up to ') ? `${n.count} iterations (it can stop early)` : `${n.count} iterations`;
    lines.push(`Created in a loop of ${times}${loops ? `: ${loops}` : ''}.`);
  }
  for (const s of n.starts ?? []) {
    const on = s.path ? s.on : `\`${s.on}\``;
    lines.push(`Started on ${on} in \`${s.phase}\`${s.conditions?.length ? `, when ${conditionsText(s.conditions)}` : ''}.`);
  }
  if (n.allocated === 'elsewhere') {
    lines.push('Made elsewhere (a handle from the config DB, an argument or a base class).');
  }
  if (n.kind === 'sequence' && !(n.starts ?? []).length) {
    lines.push('Not started in this class.');
  }
  for (const note of n.notes ?? []) {
    lines.push(note.charAt(0).toUpperCase() + note.slice(1) + '.');
  }
  if (n.ports_hidden) {
    lines.push(`${n.ports_hidden} TLM port${n.ports_hidden === 1 ? '' : 's'} hidden.`);
  }
  return lines.filter((l) => l !== '').join('\n\n');
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
  position?: { index: number; count: number; more?: boolean };
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
    parts.push(`${s.position.index + 1} of ${s.position.count}${s.position.more ? '+' : ''}`);
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

/** A Structure node, as `dvicode/structure` gives it. */
export interface StructNode {
  id: string;
  name: string;
  kind: string;
  detail?: string;
  extent?: { uri: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } };
  target?: { uri: string; range: { start: { line: number; character: number } } };
  /** An extern method's body, elsewhere in the file (or another). */
  body?: { uri: string; range: { start: { line: number; character: number }; end: { line: number; character: number } } };
  inherited_from?: string;
  mark?: string;
  inactive?: boolean;
  collapsed?: boolean;
  children: StructNode[];
}

/**
 * The innermost Structure node (not a group) whose extent is in the document (`inDoc` tells for
 * an engine URI) and contains the position; with the nodes from the top down to it.
 */
export function innermostAt(nodes: StructNode[], inDoc: (uri: string) => boolean, line: number, character: number): StructNode[] {
  const within = (e: StructNode['extent']): boolean => {
    if (!e || !inDoc(e.uri)) {
      return false;
    }
    const { start, end } = e.range;
    const afterStart = line > start.line || (line === start.line && character >= start.character);
    const beforeEnd = line < end.line || (line === end.line && character <= end.character);
    return afterStart && beforeEnd;
  };
  const inside = (n: StructNode): boolean => within(n.extent) || within(n.body);
  for (const n of nodes) {
    // Groups have no extent: look inside them.
    if (n.kind === 'group') {
      const below = innermostAt(n.children, inDoc, line, character);
      if (below.length) {
        return [n, ...below];
      }
      continue;
    }
    if (n.inherited_from) {
      continue;
    }
    if (inside(n)) {
      return [n, ...innermostAt(n.children, inDoc, line, character)];
    }
    // An extern method's body lies outside its class.
    const below = innermostAt(n.children, inDoc, line, character);
    if (below.length) {
      return [n, ...below];
    }
  }
  return [];
}

/** What the Register Model view shows of a node (`dvicode/tree` view `registers`, and the
 * fields `dvicode/registerFields` gives). */
export interface RegInfo {
  name: string;
  /** `block`, `map`, `register`, `memory` or `field`. */
  kind: string;
  class?: string;
  offset?: string;
  address?: string;
  rights?: string;
  bits?: string;
  range?: string | null;
  access?: string | null;
  reset?: string | null;
  volatile?: boolean;
  notes?: string[];
}

/** The description of a register model node: where it is, its width and access. */
export function registerDescription(n: RegInfo): string | undefined {
  let parts: (string | null | undefined)[];
  switch (n.kind) {
    case 'field':
      parts = [n.range, n.access, n.reset ? `reset ${n.reset}` : undefined];
      break;
    case 'block':
      parts = [n.address ?? (n.offset ? `offset ${n.offset}` : undefined), n.class && n.class !== n.name ? n.class : undefined];
      break;
    case 'map':
      parts = [n.address, n.bits];
      break;
    default:
      parts = [n.address ?? (n.offset ? `offset ${n.offset}` : undefined), n.bits, n.rights];
  }
  const s = parts.filter((x) => x).join(' · ');
  return (n.notes?.length ? `${s} ⚠`.trim() : s) || undefined;
}

/** The tooltip of a register model node (Markdown); `path`: the names from the top block. */
export function registerTooltip(n: RegInfo, path: string[]): string {
  const lines = [`**${n.name}** ${n.kind}${n.class ? ` : \`${n.class}\`` : ''}`, path.join('.')];
  const add = (label: string, v: string | null | undefined): void => {
    if (v) {
      lines.push(`${label}: \`${v}\``);
    }
  };
  add('Address', n.address);
  add('Offset', n.offset);
  add(n.kind === 'map' ? 'Bus width' : 'Size', n.bits);
  add('Bits', n.range);
  add('Access', n.access ?? n.rights);
  add('Reset', n.reset);
  if (n.volatile) {
    lines.push('Volatile.');
  }
  for (const note of n.notes ?? []) {
    lines.push(note.charAt(0).toUpperCase() + note.slice(1) + '.');
  }
  return lines.join('\n\n');
}

/** A result of `dvicode/checksOf`: an assertion, cover, coverage item, helper or connection. */
export interface CheckInfo {
  /** `assert`, `cover`, `coverpoint`, `property`, `port`, `argument`... */
  kind: string;
  /** `checks`, `covers`, `coverage`, `helpers` or `connections`. */
  group: string;
  form?: string;
  /** `checked`, `clock`, `reset`, `gate`, `bins`, `cross item`, `connection`, `argument`. */
  roles: string[];
  label?: string;
  detail: string;
  line: number | null;
  notes?: string[];
  children: CheckInfo[];
}

/** The groups of the Assertions and Coverage view, in order, with their titles. */
export const CHECK_GROUPS: [string, string][] = [
  ['checks', 'Assertions'],
  ['covers', 'Covers'],
  ['coverage', 'Coverage'],
  ['helpers', 'Properties and sequences'],
  ['connections', 'Through ports and arguments'],
];

/** The label of a result: its name, else what it is and where. */
export function checkLabel(h: CheckInfo): string {
  if (h.label) {
    return h.label;
  }
  const what = h.form === 'concurrent' && !h.kind.includes('sequence') && ['assert', 'assume', 'cover', 'restrict'].includes(h.kind) ? `${h.kind} property` : h.kind;
  return h.line !== null ? `${what} (line ${h.line + 1})` : what;
}

/** The description of a result: its form when not concurrent, and what the target is there
 * when it is not the checked value. */
export function checkDescription(h: CheckInfo): string | undefined {
  const parts: string[] = [];
  if (h.form === 'immediate' || h.form === 'deferred') {
    parts.push(h.form);
  }
  const roles = h.roles.filter((r) => r !== 'checked' && r !== 'connection' && r !== 'argument');
  if (roles.length) {
    parts.push(roles.join(', '));
  }
  if (h.notes?.length) {
    parts.push(h.notes.join('; '));
  }
  return parts.join(' · ') || undefined;
}

/** The tooltip of a result (Markdown). */
export function checkTooltip(h: CheckInfo): string {
  const lines = [`**${checkLabel(h)}** ${h.kind}${h.form ? ` (${h.form})` : ''}`];
  if (h.detail) {
    lines.push('```systemverilog\n' + h.detail + '\n```');
  }
  lines.push(`As: ${h.roles.join(', ')}`);
  for (const n of h.notes ?? []) {
    lines.push(n.charAt(0).toUpperCase() + n.slice(1) + '.');
  }
  return lines.join('\n\n');
}

/** Without what uses the target only as a clock or a reset ("values only"): a result goes when
 * all its roles are those; a connection goes when nothing is left under it. */
export function valuesOnly<H extends CheckInfo>(hits: H[]): H[] {
  const out: H[] = [];
  for (const h of hits) {
    const children = valuesOnly(h.children as H[]);
    const timing = h.roles.length > 0 && h.roles.every((r) => r === 'clock' || r === 'reset');
    if (h.group === 'helpers' && timing) {
      // Used there only as a clock or a reset: so is it in whatever uses the helper.
      continue;
    }
    if (h.group === 'connections' || h.group === 'helpers') {
      if (children.length || (h.group === 'helpers' && !h.children.length)) {
        out.push({ ...h, children });
      }
    } else if (!timing) {
      out.push({ ...h, children });
    }
  }
  return out;
}

/** The view's message for a result: nothing found, or what limited the search. */
export function checksMessage(name: string, hits: number, limits: string[], notes: string[]): string | undefined {
  const why: Record<string, string> = { candidates: 'too many names to look at', hits: 'too many results', depth: 'connections followed only 3 levels deep', cancelled: 'the search was cancelled' };
  const limited = limits.map((l) => why[l] ?? l);
  const parts: string[] = [];
  if (!hits) {
    parts.push(`No assertion or coverage uses '${name}' in this build configuration.`);
  }
  if (limited.length) {
    parts.push(`Search incomplete: ${limited.join(', ')}.`);
  }
  if (notes.length) {
    const shown = notes.slice(0, 3).join('; ');
    const more = notes.length > 3 ? `; and ${notes.length - 3} more` : '';
    parts.push(`${shown.charAt(0).toUpperCase()}${shown.slice(1)}${more}.`);
  }
  return parts.join(' ') || undefined;
}

/** A result of `dvicode/accessesOf`: a write, a read or a connection followed. */
export interface AccessInfo {
  /** `blocking`, `nonblocking`, `continuous`, `read`, `argument`, `gate output`, `port`... */
  kind: string;
  /** `writes`, `reads`, `control`, `connections` or `other`. */
  group: string;
  process?: string;
  label: string;
  detail: string;
  /** The instances on the way (`u_core.u_fifo`). */
  context?: string;
  line: number | null;
  notes?: string[];
  children: AccessInfo[];
}

/** The groups of the Drivers and Loads view, in order, with their titles. */
export const ACCESS_GROUPS: [string, string][] = [
  ['writes', 'Writes'],
  ['reads', 'Reads'],
  ['control', 'Release and deassign'],
  ['connections', 'Through ports'],
  ['other', 'Aliases, clocking blocks and switches'],
];

/** The label of a result: a connection's port and instance, else its source line. */
export function accessLabel(s: AccessInfo): string {
  return s.group === 'connections' || !s.detail ? s.label : s.detail;
}

/** The description of a result: what it is, where, and its notes. */
export function accessDescription(s: AccessInfo): string | undefined {
  const parts: string[] = [];
  if (s.group !== 'connections') {
    parts.push(s.label);
  }
  if (s.process) {
    parts.push(`in ${s.process}`);
  }
  if (s.context) {
    parts.push(s.context);
  }
  if (s.notes?.length) {
    parts.push(s.notes.join('; '));
  }
  return parts.join(' · ') || undefined;
}

/** The tooltip of a result (Markdown). */
export function accessTooltip(s: AccessInfo): string {
  const lines = [`**${s.label}**${s.process ? ` in ${s.process}` : ''}${s.line !== null ? ` (line ${s.line + 1})` : ''}`];
  if (s.detail) {
    lines.push('```systemverilog\n' + s.detail + '\n```');
  }
  if (s.context) {
    lines.push(`Through instances: ${s.context}`);
  }
  for (const n of s.notes ?? []) {
    lines.push(n.charAt(0).toUpperCase() + n.slice(1) + '.');
  }
  return lines.join('\n\n');
}

/** The view's message for a result: nothing found, or what limited the search. */
export function accessesMessage(name: string, mode: string, sites: number, limits: string[], notes: string[]): string | undefined {
  const why: Record<string, string> = {
    candidates: 'too many names to look at',
    sites: 'too many results',
    connections: 'too many connections',
    instances: 'too many instantiations',
    depth: 'connections followed only 3 levels deep',
    cancelled: 'the search was cancelled',
  };
  const limited = limits.map((l) => why[l] ?? l);
  const parts: string[] = [];
  if (!sites) {
    parts.push(`Nothing ${mode === 'writes' ? 'writes' : 'reads'} '${name}' in this build configuration.`);
  }
  if (limited.length) {
    parts.push(`Search incomplete: ${limited.join(', ')}.`);
  }
  if (notes.length) {
    const shown = notes.slice(0, 3).join('; ');
    const more = notes.length > 3 ? `; and ${notes.length - 3} more` : '';
    parts.push(`${shown.charAt(0).toUpperCase()}${shown.slice(1)}${more}.`);
  }
  return parts.join(' ') || undefined;
}
