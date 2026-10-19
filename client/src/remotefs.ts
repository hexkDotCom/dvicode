// `dvicode://<wsl+distro|ssh+host|tcp+addr>/<absolute path>`: files that live only on the
// server machine, read and written through the language server connection.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';

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
      if (!served || served.toLowerCase() !== u.authority.toLowerCase()) {
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
  }

  async rename(oldUri: vscode.Uri, newUri: vscode.Uri, options: { overwrite: boolean }): Promise<void> {
    await this.call('rename', { from: oldUri.path, to: newUri.path, overwrite: options.overwrite }, oldUri, newUri);
    this.emitter.fire([
      { type: vscode.FileChangeType.Deleted, uri: oldUri },
      { type: vscode.FileChangeType.Created, uri: newUri },
    ]);
  }
}
