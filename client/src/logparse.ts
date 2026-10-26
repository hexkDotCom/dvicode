// Links in simulator logs: source locations and UVM component paths, read from UVM report
// lines (the UVM library's own format, the same for every simulator), VCS messages, and the
// common `file(line)` / `file:line` forms of other tools. No `vscode` here: unit-tested in Node.

/** A link in a line or a text: offsets of what is underlined, and where it goes. */
export type LogLink =
  | { kind: 'file'; start: number; end: number; path: string; line: number }
  | { kind: 'component'; start: number; end: number; path: string };

/** Extensions of source files named outside quotes (`x.sv(12)`, `x.sv:12`). */
const SOURCE = '(?:sv|svh|svi|svp|sva|v|vh|vp|vg|vhd|vhdl|inc|pkg)';
/** A path outside quotes: no white space, quotes, parentheses, commas, colons (but a drive's),
 * or `|`. */
const PATH = `((?:[A-Za-z]:)?[^\\s"'(),:;|]+\\.${SOURCE})`;

// `UVM_ERROR ../tb/sb.sv(142) @ 1500: uvm_test_top.env.sb [SB] ...`; the file part is absent
// in reports without one (`UVM_INFO @ 0: reporter [RNTST] ...`).
const UVM = /\bUVM_(?:INFO|WARNING|ERROR|FATAL)\s+(?:(\S+?)\((\d+)\)\s+)?@\s*[^:\n]*?:\s+(\S+)\s+\[/g;
// `"x.sv", 12` (VCS syntax errors, `$error`, assertions): any extension inside quotes.
const QUOTED = /"([^"\n]+\.[A-Za-z0-9]{1,6})",\s*(\d+)/g;
// `x.sv(12)` (Questa, UVM), `x.sv:12` and `x.sv:12:3`, `x.sv,12|3` (Xcelium).
const PAREN = new RegExp(`${PATH}\\((\\d+)\\)`, 'g');
const COLON = new RegExp(`${PATH}:(\\d+)\\b`, 'g');
const XCELIUM = new RegExp(`${PATH},(\\d+)\\|\\d+`, 'g');
// A VCS message header: its location is on the next line, `path, line`.
const VCS_HEADER = /^\s*(?:Error|Warning|Lint|Note|Information)-\[[^\]]*\]/;
const VCS_LOCATION = /^\s*(\S[^,]*?),\s*(\d+)\s*$/;
// Elaboration tasks: `location: file` / `path` / `line N`, or all on one line.
const TASK_FILE = /^\s*location:\s*file\s*$/;
const TASK_ONE_LINE = /location:\s*file\s+(\S+)\s+line\s+(\d+)/;
const TASK_LINE = /^\s*line\s+(\d+)\b/;

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

/** The links of one line (offsets in the line). */
export function lineLinks(line: string): LogLink[] {
  const out: LogLink[] = [];
  for (const m of line.matchAll(UVM)) {
    const at = m.index ?? 0;
    if (m[1] !== undefined) {
      const start = at + m[0].indexOf(m[1]);
      add(out, { kind: 'file', start, end: start + m[1].length + m[2].length + 2, path: m[1], line: Number(m[2]) });
    }
    const comp = componentOf(m[3]);
    if (comp) {
      const start = at + m[0].lastIndexOf(m[3]);
      add(out, { kind: 'component', start, end: start + comp.length, path: comp });
    }
  }
  for (const m of line.matchAll(QUOTED)) {
    const at = m.index ?? 0;
    add(out, { kind: 'file', start: at, end: at + m[0].length, path: m[1], line: Number(m[2]) });
  }
  for (const re of [PAREN, XCELIUM, COLON]) {
    for (const m of line.matchAll(re)) {
      const at = m.index ?? 0;
      add(out, { kind: 'file', start: at, end: at + m[0].length, path: m[1], line: Number(m[2]) });
    }
  }
  const one = TASK_ONE_LINE.exec(line);
  if (one) {
    const start = line.indexOf(one[1], one.index);
    add(out, { kind: 'file', start, end: start + one[1].length, path: one[1], line: Number(one[2]) });
  }
  return out.sort((a, b) => a.start - b.start);
}

/** A line that may hold a link: every form has a line number after `(`, `:`, `,` or `"`, or is
 * an elaboration task's `location:`. Most lines of a long log have none and skip the rest. */
const MAYBE = /[(:,"|]\s*\d|location:/;

/** Reads the links of a whole log (offsets in `text`) a part at a time, so that a long log is
 * read without holding the thread: with the forms that span lines (a VCS message's location on
 * the line after its header; an elaboration task's file and line on their own lines); at most
 * `max`. */
export class LinkScanner {
  readonly links: LogLink[] = [];
  private pos = 0;
  // The line before was a VCS message header / `location: file` (and the path after it).
  private header = false;
  private taskFile = false;
  private taskPath: { start: number; end: number; path: string } | undefined;

  constructor(
    private readonly text: string,
    private readonly max: number,
  ) {}

  get done(): boolean {
    return this.pos > this.text.length || this.links.length >= this.max;
  }

  /** Reads up to `lines` more lines; whether the log is read through. */
  step(lines: number): boolean {
    const { text, links: out, max } = this;
    for (let k = 0; k < lines && !this.done; k++) {
      const pos = this.pos;
      let nl = text.indexOf('\n', pos);
      if (nl < 0) {
        nl = text.length;
      }
      const raw = text.slice(pos, nl);
      const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
      const found = MAYBE.test(line) ? lineLinks(line) : [];
      if (this.header && !found.length) {
        const m = VCS_LOCATION.exec(line);
        // A path: no white space in it, and a file name with an extension (not prose).
        if (m && !/\s/.test(m[1]) && /\.[A-Za-z0-9]{1,6}$/.test(m[1])) {
          const start = line.indexOf(m[1]);
          found.push({ kind: 'file', start, end: line.trimEnd().length, path: m[1], line: Number(m[2]) });
        }
      }
      if (this.taskPath) {
        const m = TASK_LINE.exec(line);
        if (m) {
          out.push({ kind: 'file', start: this.taskPath.start, end: this.taskPath.end, path: this.taskPath.path, line: Number(m[1]) });
        }
        this.taskPath = undefined;
      } else if (this.taskFile && line.trim()) {
        const start = pos + line.length - line.trimStart().length;
        this.taskPath = { start, end: pos + line.trimEnd().length, path: line.trim() };
      }
      this.taskFile = TASK_FILE.test(line);
      for (const l of found) {
        if (out.length >= max) {
          break;
        }
        out.push({ ...l, start: l.start + pos, end: l.end + pos });
      }
      // A header's location may follow a blank line; anything else ends the wait.
      this.header = VCS_HEADER.test(line) || (this.header && !line.trim());
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

/** The test a log ran: UVM's `[RNTST] Running test NAME...`. */
export function logTest(text: string): string | undefined {
  return /\[RNTST\]\s+Running test\s+([A-Za-z_][\w$]*)/.exec(text)?.[1];
}
