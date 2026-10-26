// Compile This File As...: the compile context a file that several roots or invocations compile
// is read in. The choices are the engine's to apply (each snapshot resolves them); the
// extension keeps them in the workspace state, as uris of this machine, and gives them to every
// engine that starts (a restart after a crash included).

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import type { Mapper } from './pathmap';
import type { Conn } from './views';
import { contextPickRows, contextStatus, othersKept, type ContextListInfo, type ContextPickRow, type ContextRef } from './viewmodel';

const STATE_KEY = 'dvicode.compileContexts';
/** Choices kept at most (the oldest go, in the engine too). */
const MAX_KEPT = 200;
/** Contexts asked for at a time. */
const PAGE = 200;

/** A choice as kept: the file and the root as uris of this machine. */
interface Kept {
  file: string;
  root: string;
  invocation_key: string;
  nth: number;
  before?: string | null;
  at: number;
}

export class ContextChooser implements vscode.Disposable {
  private readonly subs: vscode.Disposable[] = [];
  private readonly item: vscode.StatusBarItem;
  /** The engine whose choices are in, when it can choose contexts. */
  private engine: LanguageClient | undefined;
  /** Counts status requests: an answer to an older one is dropped. */
  private statusSeq = 0;
  /** The open picker asks again for what it shows (the analysis changed). */
  private reloadPicker: (() => void) | undefined;
  /** Closes the open picker (its engine stopped). */
  private closePicker: (() => void) | undefined;

  constructor(
    private readonly state: vscode.Memento,
    private readonly conn: Conn,
  ) {
    this.item = vscode.window.createStatusBarItem('dvicode.compileContext', vscode.StatusBarAlignment.Left, 9);
    this.item.name = 'DVICode Compile Context';
    this.item.command = 'dvicode.selectCompileContext';
    this.subs.push(
      this.item,
      vscode.commands.registerCommand('dvicode.selectCompileContext', (arg?: unknown, choice?: ContextRef | null) => this.select(arg, choice)),
      vscode.window.onDidChangeActiveTextEditor(() => this.refresh()),
    );
  }

  dispose(): void {
    for (const s of this.subs) {
      s.dispose();
    }
  }

  /**
   * An engine is running (started, or restarted after a crash): it gets every choice kept,
   * before the picker and the status item use it.
   */
  async started(c: LanguageClient, supported: boolean): Promise<void> {
    this.engine = undefined;
    this.item.hide();
    if (!supported) {
      return;
    }
    const m = this.conn.mapper();
    if (m) {
      try {
        await c.sendRequest('dvicode/restoreCompileContexts', { entries: this.entries(this.kept(), m) });
      } catch {
        // Gone already: the next start sends them again.
        return;
      }
    }
    this.engine = c;
    this.refresh();
  }

  /** The engine stopped. */
  stopped(): void {
    this.engine = undefined;
    this.statusSeq++;
    this.item.hide();
    this.closePicker?.();
  }

  /** The analysis changed, or the active editor: the status item and an open picker ask again. */
  refresh(): void {
    this.reloadPicker?.();
    const seq = ++this.statusSeq;
    const c = this.client();
    const m = this.conn.mapper();
    const doc = vscode.window.activeTextEditor?.document;
    if (!c || !m || !doc || !['systemverilog', 'verilog'].includes(doc.languageId)) {
      this.item.hide();
      return;
    }
    c.sendRequest<ContextListInfo>('dvicode/compileContexts', { textDocument: { uri: m.toServer(doc.uri) }, limit: 0 }).then(
      (l) => {
        if (seq !== this.statusSeq) {
          return;
        }
        const s = l && contextStatus(l, (u) => this.nameOf(m, u));
        if (!s) {
          this.item.hide();
          return;
        }
        this.item.text = s.text;
        this.item.tooltip = s.tooltip;
        this.item.show();
      },
      () => {
        if (seq === this.statusSeq) {
          this.item.hide();
        }
      },
    );
  }

  /** The engine in use, when its choices are in. */
  private client(): LanguageClient | undefined {
    const c = this.conn.client();
    return c && c === this.engine ? c : undefined;
  }

  private nameOf(m: Mapper, uri: string): string {
    const u = m.toClient(uri);
    return vscode.workspace.asRelativePath(u, false) || u.path;
  }

  private kept(): Kept[] {
    const v = this.state.get<Kept[]>(STATE_KEY);
    return Array.isArray(v) ? v.filter((k) => k && typeof k.file === 'string' && typeof k.root === 'string') : [];
  }

  private entries(kept: Kept[], m: Mapper): { uri: string; context: ContextRef }[] {
    return kept.map((k) => ({
      uri: m.toServer(vscode.Uri.parse(k.file)),
      context: { root: m.toServer(vscode.Uri.parse(k.root)), invocation_key: k.invocation_key, nth: k.nth, before: k.before ?? null },
    }));
  }

  /**
   * Keeps the choice of `file` in place of those of `aliases` (other spellings of it, as the
   * engine names them); the oldest go past the limit, from the engine too.
   */
  private async keep(c: LanguageClient, file: vscode.Uri, choice: ContextRef | null, aliases: string[], m: Mapper): Promise<void> {
    const key = file.toString();
    const serverOf = (f: string): string => m.toServer(vscode.Uri.parse(f));
    let v = othersKept(this.kept(), key, aliases, serverOf, (u) => vscode.Uri.parse(u).toString());
    let evicted = false;
    if (choice) {
      v.push({ file: key, root: m.toClient(choice.root).toString(), invocation_key: choice.invocation_key, nth: choice.nth, before: choice.before ?? null, at: Date.now() });
      v.sort((a, b) => a.at - b.at);
      evicted = v.length > MAX_KEPT;
      v = v.slice(Math.max(0, v.length - MAX_KEPT));
    }
    await this.state.update(STATE_KEY, v);
    if (evicted) {
      await c.sendRequest('dvicode/restoreCompileContexts', { entries: this.entries(v, m) }).then(undefined, () => undefined);
    }
  }

  /** The file a command is about: a uri, a Compiled Files item, else the active editor's. */
  private sourceOf(arg: unknown): vscode.Uri | undefined {
    if (arg instanceof vscode.Uri) {
      return arg.scheme === 'dvicode-context' ? vscode.Uri.parse(arg.query) : arg;
    }
    const item = arg as { resourceUri?: vscode.Uri } | undefined;
    if (item?.resourceUri instanceof vscode.Uri) {
      return item.resourceUri;
    }
    const doc = vscode.window.activeTextEditor?.document;
    return doc?.uri.scheme === 'dvicode-context' ? vscode.Uri.parse(doc.uri.query) : doc?.uri;
  }

  /**
   * Chooses the context of a file: `choice` when given (a context's key as the engine gave it,
   * or `null` for the default), else from a picker. Whether a choice was made.
   */
  private async select(arg: unknown, choice?: ContextRef | null): Promise<boolean> {
    const source = this.sourceOf(arg);
    const c = this.client();
    const m = this.conn.mapper();
    if (!source) {
      void vscode.window.showInformationMessage('DVICode: open a SystemVerilog file to choose how it is compiled.');
      return false;
    }
    if (!c || !m) {
      void vscode.window.showInformationMessage('DVICode: the engine is not running, or it cannot choose compile contexts (an older engine).');
      return false;
    }
    if (choice === undefined) {
      const picked = await this.pick(c, m, source);
      if (picked === undefined) {
        return false;
      }
      choice = picked;
    }
    if (this.client() !== c) {
      return false;
    }
    let aliases: string[] = [];
    try {
      const r = await c.sendRequest<{ aliases?: string[] }>('dvicode/setCompileContext', { textDocument: { uri: m.toServer(source) }, context: choice });
      aliases = r?.aliases ?? [];
    } catch (e) {
      void vscode.window.showWarningMessage(`DVICode: ${e instanceof Error ? e.message : String(e)}. Choose again.`);
      return false;
    }
    await this.keep(c, source, choice, aliases, m);
    // The views, the report and the status item ask again on the revision it makes.
    return true;
  }

  /**
   * The picker: the context chosen (`null`: the default), undefined when dismissed. What is
   * typed is searched on the engine (every context, not only those shown), and each page more
   * is asked for when its row is picked.
   */
  private pick(c: LanguageClient, m: Mapper, source: vscode.Uri): Promise<ContextRef | null | undefined> {
    type Item = vscode.QuickPickItem & { row: ContextPickRow };
    const qp = vscode.window.createQuickPick<Item>();
    const base = source.path.split('/').pop() ?? '';
    qp.title = `Compile ${base} as...`;
    qp.placeholder = 'Search by root file, include, mode or "inv N"';
    // The engine filters; rows are shown as it answered.
    qp.matchOnDescription = false;
    qp.matchOnDetail = false;
    let seq = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let first = true;
    const load = async (filter: string, offset: number): Promise<void> => {
      const n = ++seq;
      qp.busy = true;
      try {
        const l = await c.sendRequest<ContextListInfo>('dvicode/compileContexts', { textDocument: { uri: m.toServer(source) }, filter: filter || undefined, offset, limit: PAGE });
        if (n !== seq || this.client() !== c) {
          return;
        }
        if (first && l.total <= 1 && l.state === 'default') {
          qp.hide();
          void vscode.window.showInformationMessage(l.total ? `DVICode: ${base} is compiled in one context only.` : `DVICode: ${base} is not compiled in this build.`);
          return;
        }
        first = false;
        qp.title = `Compile ${base} as... (${filter ? `${l.matched} of ` : ''}${l.total} context${l.total === 1 ? '' : 's'})`;
        const rows = contextPickRows(l, (u) => this.nameOf(m, u), !!filter || offset > 0).map((row) => ({
          label: row.label,
          description: row.description,
          detail: row.detail,
          alwaysShow: true,
          row,
        }));
        // A further page replaces the "more" row it was asked from.
        qp.items = offset > 0 ? [...qp.items.filter((i) => !i.row.more), ...rows] : rows;
      } catch (e) {
        if (n === seq) {
          qp.items = [{ label: `No answer from the engine: ${e instanceof Error ? e.message : String(e)}`, alwaysShow: true, row: { label: '' } }];
        }
      } finally {
        if (n === seq) {
          qp.busy = false;
        }
      }
    };
    return new Promise((resolve) => {
      let done = false;
      const finish = (v: ContextRef | null | undefined): void => {
        if (!done) {
          done = true;
          resolve(v);
          qp.hide();
        }
      };
      this.reloadPicker = () => void load(qp.value.trim(), 0);
      this.closePicker = () => qp.hide();
      qp.onDidChangeValue((v) => {
        if (timer) {
          clearTimeout(timer);
        }
        // An answer for the text before is no answer for this one.
        seq++;
        timer = setTimeout(() => void load(v.trim(), 0), 150);
      });
      qp.onDidAccept(() => {
        const row = qp.selectedItems[0]?.row;
        if (row?.more) {
          void load(qp.value.trim(), row.more);
        } else if (row && row.choice !== undefined) {
          finish(row.choice);
        }
      });
      qp.onDidHide(() => {
        if (timer) {
          clearTimeout(timer);
        }
        seq++;
        this.reloadPicker = undefined;
        this.closePicker = undefined;
        qp.dispose();
        finish(undefined);
      });
      qp.show();
      void load('', 0);
    });
  }
}
