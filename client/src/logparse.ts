// Links in simulator logs: source locations and UVM component paths, read from UVM report
// lines (the UVM report server's default format), VCS messages, and the common `file(line)` /
// `file:line` forms of other tools. No `vscode` here: unit-tested in Node.

/** A link in a line or a text: offsets of what is underlined, and where it goes. */
export type LogLink =
  | { kind: 'file'; start: number; end: number; path: string; line: number }
  | { kind: 'component'; start: number; end: number; path: string };

/** Characters of a line read at most: a longer line (a dump, a huge message) is read up to there. */
const MAX_LINE = 4096;
/** Extensions of source files named outside quotes (`x.sv(12)`, `x.sv:12`). */
const SOURCE = /\.(?:sv|svh|svi|svp|sva|v|vh|vp|vg|vhd|vhdl|inc|pkg)$/i;

// `UVM_ERROR ../tb/sb.sv(142) @ 1500: uvm_test_top.env.sb [SB] ...`, with the verbosity when
// the report server shows it (`UVM_INFO(UVM_LOW) ...`); the file part is absent in reports
// without one (`UVM_INFO @ 0: reporter [RNTST] ...`). The file may hold spaces: it ends at the
// `(line) @` that follows it.
const UVM = /\bUVM_(?:INFO|WARNING|ERROR|FATAL)(?:\([A-Z_]+\))?\s+(?:(\S[^\n]*?)\((\d+)\)\s+)?@\s*[^:\n]*?:\s+(\S+)\s+\[/g;
// `"x.sv", 12` (VCS syntax errors, `$error`, assertions): any extension inside quotes.
const QUOTED = /"([^"\n]+\.[A-Za-z0-9]{1,6})",\s*(\d+)/g;
// Anchors of the forms outside quotes, found first, the path then read back from them:
// `x.sv(12)` (Questa, UVM), `x.sv:12` and `x.sv:12:3`, `x.sv,12|3` (Xcelium).
const ANCHORS = /\((\d+)\)|:(\d+)\b|,(\d+)\|\d+/g;
// A VCS message header: its location is on the next line, `path, line`.
const VCS_HEADER = /^\s*(?:Error|Warning|Lint|Note|Information)-\[[^\]]*\]/;
const VCS_LOCATION = /^\s*(\S[^,]*?),\s*(\d+)\s*$/;
// Elaboration tasks: `location: file` / `path` / `line N`, or all on one line.
const TASK_FILE = /^\s*location:\s*file\s*$/;
const TASK_ONE_LINE = /location:\s*file\s+(\S+)\s+line\s+(\d+)/;
const TASK_LINE = /^\s*line\s+(\d+)\b/;
// Terminal colours and other escape sequences (CSI `ESC [ ... letter`, OSC `ESC ] ... BEL`).
// eslint-disable-next-line no-control-regex
const ESCAPES = /\u001b\[[0-?]*[ -/]*[@-~]|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/g;
/** A line that may hold a link: every form has a line number after `(`, `:`, `,` or `"`, or is
 * a UVM report or an elaboration task's `location:`. Most lines of a long log have none. */
const MAYBE = /[(:,"|]\s*\d|location:|UVM_/;
/** A line of an error or a warning: its links have a budget of their own. */
const SEVERE = /UVM_(?:ERROR|FATAL|WARNING)|^\s*(?:Error|Warning)[-:]|\*\* (?:Error|Warning)|\*[EWF],/;

/** Whether `a` overlaps a link already found. */
function overlaps(found: LogLink[], start: number, end: number): boolean {
  return found.some((l) => start < l.end && l.start < end);
}

function add(out: LogLink[], l: LogLink): void {
  if (l.end > l.start && !overlaps(out, l.start, l.end)) {
    out.push(l);
  }
}

/** A UVM reporter that is a component path: `uvm_test_top...`; a sequence on a sequencer
 * (`uvm_test_top.env.sqr@@seq`) is its sequencer. */
function componentOf(reporter: string): string | undefined {
  const p = reporter.split('@@')[0];
  return /^uvm_test_top(\.|$)/.test(p) ? p : undefined;
}

/** Whether `c` ends a path written outside quotes. */
function delimits(c: string): boolean {
  return /[\s"'(),:;|<>=]/.test(c);
}

/** The path written just before `end` in `line` (outside quotes): back to a delimiter, with a
 * drive (`C:`) before it; undefined unless it names a source file. */
function pathBefore(line: string, end: number): { start: number; path: string } | undefined {
  let s = end;
  while (s > 0 && !delimits(line[s - 1])) {
    s--;
  }
  // `C:\x\y.sv` or `C:/x/y.sv`: the drive's colon is part of it.
  if (s >= 2 && line[s - 1] === ':' && /[A-Za-z]/.test(line[s - 2]) && (s < 3 || delimits(line[s - 3])) && /[\\/]/.test(line[s] ?? '')) {
    s -= 2;
  }
  const path = line.slice(s, end);
  return SOURCE.test(path) ? { start: s, path } : undefined;
}

/** `line` without escape sequences, and for each of its offsets the offset in `line`. */
function stripEscapes(line: string): { text: string; at: (i: number) => number } {
  if (!line.includes('\u001b')) {
    return { text: line, at: (i) => i };
  }
  const map: number[] = [];
  let text = '';
  let last = 0;
  for (const m of line.matchAll(ESCAPES)) {
    const i = m.index ?? 0;
    for (let k = last; k < i; k++) {
      map.push(k);
    }
    text += line.slice(last, i);
    last = i + m[0].length;
  }
  for (let k = last; k <= line.length; k++) {
    map.push(k);
  }
  text += line.slice(last);
  return { text, at: (i) => map[i] ?? line.length };
}

/** The links of one line (offsets in the line). */
export function lineLinks(raw: string): LogLink[] {
  const { text: full, at } = stripEscapes(raw);
  const line = full.length > MAX_LINE ? full.slice(0, MAX_LINE) : full;
  const out: LogLink[] = [];
  for (const m of line.matchAll(UVM)) {
    const i = m.index ?? 0;
    if (m[1] !== undefined) {
      const start = i + m[0].indexOf(m[1]);
      add(out, { kind: 'file', start, end: start + m[1].length + m[2].length + 2, path: m[1], line: Number(m[2]) });
    }
    const comp = componentOf(m[3]);
    if (comp) {
      const start = i + m[0].lastIndexOf(m[3]);
      add(out, { kind: 'component', start, end: start + comp.length, path: comp });
    }
  }
  for (const m of line.matchAll(QUOTED)) {
    const i = m.index ?? 0;
    add(out, { kind: 'file', start: i, end: i + m[0].length, path: m[1], line: Number(m[2]) });
  }
  for (const m of line.matchAll(ANCHORS)) {
    const i = m.index ?? 0;
    const p = pathBefore(line, i);
    if (p) {
      add(out, { kind: 'file', start: p.start, end: i + m[0].length, path: p.path, line: Number(m[1] ?? m[2] ?? m[3]) });
    }
  }
  const one = TASK_ONE_LINE.exec(line);
  if (one) {
    const start = line.indexOf(one[1], one.index);
    add(out, { kind: 'file', start, end: start + one[1].length, path: one[1], line: Number(one[2]) });
  }
  return out.map((l) => ({ ...l, start: at(l.start), end: at(l.end) })).sort((a, b) => a.start - b.start);
}

/** Reads the links of a whole log (offsets in `text`) a part at a time, so that a long log is
 * read without holding the thread: with the forms that span lines (a VCS message's location on
 * the line after its header; an elaboration task's file and line on their own lines). At most
 * `max` links of ordinary lines, and `max` more of errors and warnings (a log full of reports
 * still has its failures linked). */
export class LinkScanner {
  readonly links: LogLink[] = [];
  private pos = 0;
  private ordinary = 0;
  private severe = 0;
  // The line before was a VCS message header (and whether it was an error or a warning) /
  // `location: file` (and the path after it).
  private header = false;
  private headerSevere = false;
  private taskFile = false;
  private taskPath: { start: number; end: number; path: string; severe: boolean } | undefined;
  private lastSevere = false;

  constructor(
    private readonly text: string,
    private readonly max: number,
  ) {}

  get done(): boolean {
    return this.pos > this.text.length || (this.ordinary >= this.max && this.severe >= this.max);
  }

  private push(l: LogLink, severe: boolean): void {
    if (severe ? this.severe < this.max : this.ordinary < this.max) {
      this.links.push(l);
      if (severe) {
        this.severe++;
      } else {
        this.ordinary++;
      }
    }
  }

  /** Reads up to `lines` more lines, and up to about `bytes` (long lines); whether the log is
   * read through. */
  step(lines: number, bytes = Infinity): boolean {
    const text = this.text;
    const until = this.pos + bytes;
    for (let k = 0; k < lines && !this.done && this.pos < until; k++) {
      const pos = this.pos;
      let nl = text.indexOf('\n', pos);
      if (nl < 0) {
        nl = text.length;
      }
      const raw = text.slice(pos, Math.min(nl, pos + MAX_LINE * 4));
      const full = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
      const stripped = stripEscapes(full);
      const plain = stripped.text;
      // A line's links count as an error's when it is one, or a VCS error's location.
      const severe = SEVERE.test(plain) || (this.header && this.headerSevere);
      const found = MAYBE.test(plain) ? lineLinks(full) : [];
      if (this.header && !found.length) {
        const m = VCS_LOCATION.exec(plain);
        // A path, not prose: it ends with a file extension that starts with a letter (not a
        // number such as `2.0`); it may hold spaces (`C:\work dir\x.sv, 12`, `my file.sv, 3`).
        // Found in the text without escapes, its offsets mapped back to the line as written.
        if (m && /\.[A-Za-z][A-Za-z0-9]{0,5}$/.test(m[1])) {
          const start = plain.indexOf(m[1]);
          found.push({ kind: 'file', start: stripped.at(start), end: stripped.at(plain.trimEnd().length), path: m[1], line: Number(m[2]) });
        }
      }
      if (this.taskPath) {
        const m = TASK_LINE.exec(plain);
        if (m) {
          this.push({ kind: 'file', start: this.taskPath.start, end: this.taskPath.end, path: this.taskPath.path, line: Number(m[1]) }, this.taskPath.severe);
        }
        this.taskPath = undefined;
      } else if (this.taskFile && plain.trim()) {
        const lead = full.length - full.trimStart().length;
        this.taskPath = { start: pos + lead, end: pos + full.trimEnd().length, path: plain.trim(), severe: this.lastSevere };
      }
      this.taskFile = TASK_FILE.test(plain);
      for (const l of found) {
        this.push({ ...l, start: l.start + pos, end: l.end + pos }, severe);
      }
      // A header's location may follow a blank line; anything else ends the wait.
      if (VCS_HEADER.test(plain)) {
        this.header = true;
        this.headerSevere = /^\s*(?:Error|Warning)/.test(plain);
      } else {
        this.header = this.header && !plain.trim();
      }
      if (plain.trim()) {
        this.lastSevere = severe || /^\s*(?:Error|Warning)-/.test(plain) || (this.lastSevere && /^\s/.test(plain));
      }
      this.pos = nl + 1;
    }
    return this.done;
  }
}

/** The links of a whole log at once (see [`LinkScanner`]), in order. */
export function textLinks(text: string, max: number): LogLink[] {
  const s = new LinkScanner(text, max);
  s.step(Infinity);
  return s.links.sort((a, b) => a.start - b.start);
}

/** The test a log ran: UVM's `[RNTST] Running test NAME...`; with `before`, the last one
 * before that offset (a log of several runs one after the other). */
export function logTest(text: string, before = Infinity): string | undefined {
  let test: string | undefined;
  for (const m of text.matchAll(/\[RNTST\]\s+Running test\s+([A-Za-z_][\w$]*)/g)) {
    if ((m.index ?? 0) > before) {
      break;
    }
    test = m[1];
  }
  return test;
}

/** A VCS message's location on a line of its own (`rtl/top.sv, 42`), read without the header
 * line before it (a terminal gives one line at a time): only a source file's path. */
export function locationLine(line: string): LogLink | undefined {
  const { text, at } = stripEscapes(line);
  const m = VCS_LOCATION.exec(text);
  if (!m || !SOURCE.test(m[1])) {
    return undefined;
  }
  const start = text.indexOf(m[1]);
  return { kind: 'file', start: at(start), end: at(text.trimEnd().length), path: m[1], line: Number(m[2]) };
}
