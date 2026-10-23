// Internal errors and crashes of the engine: notices with Report Issue and Show Log, and a
// pre-filled issue for VS Code's issue reporter. Nothing is sent anywhere unless the user
// submits the issue.

import * as vscode from 'vscode';
import { CloseAction, CloseHandlerResult, ErrorAction, ErrorHandler, ErrorHandlerResult } from 'vscode-languageclient/node';

/** `dvicode/internalError` from the engine. */
export interface InternalError {
  feature: string;
  message: string;
  location: string;
  backtrace: string;
  version: string;
}

/** What a report says about the setup. */
export interface ReportContext {
  extensionVersion: string;
  engineVersion?: string;
  target?: string;
  mode?: string;
  buildFile?: string | null;
  files?: number;
}

const LOG_LINES = 100;
const BACKTRACE_LINES = 40;
/** The engine is restarted after a crash at most this often within CRASH_WINDOW_MS. */
const MAX_RESTARTS = 4;
const CRASH_WINDOW_MS = 3 * 60_000;

/** "textDocument/semanticTokens/full" -> "semantic tokens full". */
export function featureName(feature: string): string {
  return feature
    .replace(/^(textDocument|workspace|dvicode)\//, '')
    .replace(/\//g, ' ')
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .toLowerCase();
}

/** Output channel that also keeps its last lines for issue reports. */
export class LogChannel implements vscode.OutputChannel {
  private lines: string[] = [];
  private partial = '';

  constructor(private readonly channel: vscode.OutputChannel) {}

  get name(): string {
    return this.channel.name;
  }

  append(value: string): void {
    this.channel.append(value);
    this.record(value);
  }

  appendLine(value: string): void {
    this.channel.appendLine(value);
    this.record(value + '\n');
  }

  replace(value: string): void {
    this.channel.replace(value);
    this.lines = [];
    this.partial = '';
    this.record(value);
  }

  clear(): void {
    this.channel.clear();
    this.lines = [];
    this.partial = '';
  }

  show(preserveFocus?: boolean): void;
  show(column?: vscode.ViewColumn, preserveFocus?: boolean): void;
  show(a?: boolean | vscode.ViewColumn, preserveFocus?: boolean): void {
    if (typeof a === 'boolean' || a === undefined) {
      this.channel.show(a);
    } else {
      this.channel.show(a, preserveFocus);
    }
  }

  hide(): void {
    this.channel.hide();
  }

  dispose(): void {
    this.channel.dispose();
  }

  /** The last lines written. */
  tail(n: number): string[] {
    const all = this.partial ? [...this.lines, this.partial] : this.lines;
    return all.slice(-n);
  }

  private record(text: string): void {
    const parts = (this.partial + text).split('\n');
    this.partial = parts.pop() ?? '';
    this.lines.push(...parts);
    if (this.lines.length > LOG_LINES * 2) {
      this.lines.splice(0, this.lines.length - LOG_LINES);
    }
  }
}

export class Reporter {
  last: InternalError | undefined;
  /** Notices already shown in this window (feature names, or "crash"). */
  private shown = new Set<string>();
  private crashes: number[] = [];

  constructor(
    private readonly log: LogChannel,
    private readonly context: () => ReportContext,
    private readonly extensionId: string,
  ) {}

  onInternalError(e: InternalError): void {
    this.last = e;
    this.log.appendLine(`Internal error in ${featureName(e.feature)}: ${e.message} (${e.location})`);
    this.notify(e.feature, `DVICode: internal error in ${featureName(e.feature)}. Results may be incomplete.`);
  }

  /**
   * Restarts the engine after a crash, a few times, and offers to report it; never the engine
   * of a client that is `replaced`.
   */
  errorHandler(replaced: () => boolean = () => false): ErrorHandler {
    return {
      error: (_error, _message, count): ErrorHandlerResult => ({
        action: !replaced() && (count ?? 0) <= 3 ? ErrorAction.Continue : ErrorAction.Shutdown,
      }),
      closed: (): CloseHandlerResult => {
        if (replaced()) {
          return { action: CloseAction.DoNotRestart, handled: true };
        }
        const now = Date.now();
        this.crashes = this.crashes.filter((t) => now - t < CRASH_WINDOW_MS);
        this.crashes.push(now);
        if (this.crashes.length <= MAX_RESTARTS) {
          this.log.appendLine('The DVICode engine stopped unexpectedly; restarting it.');
          this.notify('crash', 'DVICode: the engine stopped unexpectedly and was restarted.');
          return { action: CloseAction.Restart, handled: true };
        }
        this.log.appendLine('The DVICode engine stopped repeatedly; not restarting it.');
        this.shown.delete('crash');
        this.notify('crash', 'DVICode: the engine stopped 5 times in 3 minutes and was not restarted. Use the status bar item to start it again.', true);
        return { action: CloseAction.DoNotRestart, handled: true };
      },
    };
  }

  /** The issue text: setup, last internal error and the end of the log. */
  issueBody(): string {
    const c = this.context();
    const fence = '```';
    const parts = [
      '<!-- What were you doing, and what went wrong? A minimal code sample that shows it helps most. -->',
      '<!-- Please review the details below before submitting: the log can contain file and folder names. -->',
      '',
      '### What happened',
      '',
      '',
      '### Setup',
      '',
      `- DVICode ${c.extensionVersion}, engine ${c.engineVersion ?? 'not running'}`,
      `- VS Code ${vscode.version} (${vscode.env.appName}${vscode.env.remoteName ? `, remote ${vscode.env.remoteName}` : ''})`,
      `- ${process.platform} ${process.arch}`,
      `- Engine: ${c.target ?? 'not started'}${c.mode ? ` (mode ${c.mode})` : ''}`,
      `- Build: ${c.buildFile ?? 'auto-configuration'}${c.files !== undefined ? `, ${c.files} files` : ''}`,
    ];
    const e = this.last;
    if (e) {
      const bt = e.backtrace.split('\n').slice(0, BACKTRACE_LINES).join('\n');
      parts.push('', '### Internal error', '', `${featureName(e.feature)}: ${e.message}`, `at ${e.location} (engine ${e.version})`, '');
      parts.push('<details><summary>Backtrace</summary>', '', fence, bt, fence, '', '</details>');
    }
    parts.push('', `<details><summary>Log (last ${LOG_LINES} lines)</summary>`, '', fence, ...this.log.tail(LOG_LINES), fence, '', '</details>', '');
    return parts.join('\n');
  }

  /** Opens VS Code's issue reporter with the report filled in; if that is not available,
   *  copies the report and opens a new issue on GitHub. */
  async reportIssue(): Promise<void> {
    const body = this.issueBody();
    const title = this.last ? `Internal error in ${featureName(this.last.feature)}` : '';
    try {
      await vscode.commands.executeCommand('workbench.action.openIssueReporter', {
        extensionId: this.extensionId,
        issueTitle: title,
        issueBody: body,
      });
    } catch {
      const bugs = vscode.extensions.getExtension(this.extensionId)?.packageJSON?.bugs?.url as string | undefined;
      await vscode.env.clipboard.writeText(body);
      if (bugs) {
        await vscode.env.openExternal(vscode.Uri.parse(`${bugs}/new?template=bug_report.yml`));
      }
      void vscode.window.showInformationMessage('DVICode: the report is on the clipboard; paste it into the new issue.');
    }
  }

  private notify(key: string, message: string, error = false): void {
    if (this.shown.has(key)) {
      return;
    }
    this.shown.add(key);
    const show = error ? vscode.window.showErrorMessage : vscode.window.showWarningMessage;
    void show(message, 'Report Issue', 'Show Log').then((a) => {
      if (a === 'Report Issue') {
        void this.reportIssue();
      } else if (a === 'Show Log') {
        this.log.show();
      }
    });
  }
}
