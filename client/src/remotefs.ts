// `dvicode://<wsl+distro|ssh+host|tcp+addr>/<absolute path>`: files that live only on the
// server machine, read and written through the language server connection.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';

/**
 * Whether two dvicode:// authorities name the same server: host names ignore case, and
 * "Open Folder on the Server" writes `ssh+user%40host` where the settings give `ssh+user@host`.
 */
export function sameAuthority(a: string, b: string): boolean {
  // Host names and WSL distributions ignore case; an SSH login name does not.
  const lower = (s: string): string => {
    const m = /^ssh\+([^@]*)@(.*)$/i.exec(s);
    return m ? `ssh+${m[1]}@${m[2].toLowerCase()}` : s.toLowerCase();
  };
  const canon = (s: string) => {
    try {
      return lower(decodeURIComponent(s));
    } catch {
      return lower(s);
    }
  };
  return canon(a) === canon(b);
}

export class RemoteFs implements vscode.FileSystemProvider {
  private readonly emitter = new vscode.EventEmitter<vscode.FileChangeEvent[]>();
  readonly onDidChangeFile = this.emitter.event;

  /**
   * `client` starts the language server if needed; `authority` is the dvicode:// authority that
   * server serves (`wsl+…`, `ssh+…`, `tcp+…`), or undefined when it runs on this machine.
   */
  constructor(
    private readonly client: () => Promise<LanguageClient>,
    private readonly authority: () => string | undefined,
  ) {}

  private async call<T>(op: string, params: object, uri: vscode.Uri, other?: vscode.Uri): Promise<T> {
    const c = await this.client();
    // One server per window: a folder of another host must not be read or written through it.
    const served = this.authority();
    for (const u of other ? [uri, other] : [uri]) {
      if (!served || !sameAuthority(served, u.authority)) {
        throw vscode.FileSystemError.Unavailable(
          `DVICode is connected to ${served ? `dvicode://${served}` : 'this machine'}, not dvicode://${u.authority}; open folders of one server per window`,
        );
      }
    }
    try {
      return await c.sendRequest<T>(`dvicode/fs/${op}`, params);
    } catch (e: unknown) {
      const err = e as { data?: { code?: string }; message?: string };
      switch (err?.data?.code) {
        case 'FileNotFound':
          throw vscode.FileSystemError.FileNotFound(uri);
        case 'FileExists':
          throw vscode.FileSystemError.FileExists(uri);
        case 'NoPermissions':
          throw vscode.FileSystemError.NoPermissions(uri);
        case 'FileIsADirectory':
          throw vscode.FileSystemError.FileIsADirectory(uri);
        case 'FileNotADirectory':
          throw vscode.FileSystemError.FileNotADirectory(uri);
        default:
          throw vscode.FileSystemError.Unavailable(err?.message ?? String(e));
      }
    }
  }

  /**
   * Tells the server itself about a deleted or renamed path: the editor's watcher reports
   * only source and build files, not a folder of them (`rtl/`), whose files the server forgets
   * or looks for again from this.
   */
  private async moved(changes: { path: string; type: number }[]): Promise<void> {
    try {
      const c = await this.client();
      await c.sendNotification('workspace/didChangeWatchedFiles', {
        changes: changes.map((x) => ({ uri: vscode.Uri.from({ scheme: 'file', path: x.path }).toString(), type: x.type })),
      });
    } catch {
      // The server is gone: it reads everything again when it starts.
    }
  }

  watch(): vscode.Disposable {
    // The server watches nothing; edits made through VS Code are reported below.
    return new vscode.Disposable(() => undefined);
  }

  async stat(uri: vscode.Uri): Promise<vscode.FileStat> {
    const s = await this.call<{ type: number; size: number; mtime: number; ctime: number; readonly: boolean }>('stat', { path: uri.path }, uri);
    return {
      type: s.type as vscode.FileType,
      size: s.size,
      mtime: s.mtime,
      ctime: s.ctime,
      permissions: s.readonly ? vscode.FilePermission.Readonly : undefined,
    };
  }

  async readDirectory(uri: vscode.Uri): Promise<[string, vscode.FileType][]> {
    return this.call<[string, vscode.FileType][]>('readDirectory', { path: uri.path }, uri);
  }

  async readFile(uri: vscode.Uri): Promise<Uint8Array> {
    const r = await this.call<{ content: string }>('readFile', { path: uri.path }, uri);
    return Buffer.from(r.content, 'base64');
  }

  async writeFile(uri: vscode.Uri, content: Uint8Array, options: { create: boolean; overwrite: boolean }): Promise<void> {
    const r = await this.call<{ created?: boolean } | null>(
      'writeFile',
      { path: uri.path, content: Buffer.from(content).toString('base64'), create: options.create, overwrite: options.overwrite },
      uri,
    );
    // A new file must be reported as created: the server looks for new sources on that event.
    this.emitter.fire([{ type: r?.created ? vscode.FileChangeType.Created : vscode.FileChangeType.Changed, uri }]);
  }

  async createDirectory(uri: vscode.Uri): Promise<void> {
    await this.call('createDirectory', { path: uri.path }, uri);
    this.emitter.fire([{ type: vscode.FileChangeType.Created, uri }]);
  }

  async delete(uri: vscode.Uri, options: { recursive: boolean }): Promise<void> {
    await this.call('delete', { path: uri.path, recursive: options.recursive }, uri);
    this.emitter.fire([{ type: vscode.FileChangeType.Deleted, uri }]);
    await this.moved([{ path: uri.path, type: 3 }]);
  }

  async rename(oldUri: vscode.Uri, newUri: vscode.Uri, options: { overwrite: boolean }): Promise<void> {
    await this.call('rename', { from: oldUri.path, to: newUri.path, overwrite: options.overwrite }, oldUri, newUri);
    this.emitter.fire([
      { type: vscode.FileChangeType.Deleted, uri: oldUri },
      { type: vscode.FileChangeType.Created, uri: newUri },
    ]);
    await this.moved([
      { path: oldUri.path, type: 3 },
      { path: newUri.path, type: 1 },
    ]);
  }
}
