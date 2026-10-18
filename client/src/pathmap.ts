// Translates URIs between VS Code (this machine) and the language server (possibly WSL or a
// remote Linux host).
//
//  VS Code side                                   server side
//  file://wsl.localhost/Ubuntu/home/me/p/a.sv  <->  file:///home/me/p/a.sv     (WSL UNC path)
//  file:///d%3A/work/a.sv                      <->  file:///mnt/d/work/a.sv     (WSL, Windows drive)
//  dvicode://ssh+build01/home/me/p/a.sv        <->  file:///home/me/p/a.sv     (remote folder)
//  file:///z%3A/proj/a.sv (pathMappings)       <->  file:///home/me/proj/a.sv   (Samba/NFS/SSHFS)

import * as vscode from 'vscode';

export interface PathMapping {
  local: string;
  remote: string;
}

export interface MapperOptions {
  /** The server runs in this WSL distribution. */
  wslDistro?: string;
  /** Authority of dvicode:// URIs for files that only exist on the server. */
  remoteAuthority?: string;
  mappings: PathMapping[];
}

export interface Mapper {
  toServer(uri: vscode.Uri): string;
  toClient(uri: string): vscode.Uri;
  /** Server path for a local URI, if it maps. */
  serverPath(uri: vscode.Uri): string | undefined;
  describe(): string;
}

const isWin = process.platform === 'win32';

function norm(p: string): string {
  let s = p.replace(/\\/g, '/');
  if (s.length > 1 && s.endsWith('/')) {
    s = s.slice(0, -1);
  }
  return s;
}

function startsWithPath(p: string, prefix: string): boolean {
  const a = isWin ? p.toLowerCase() : p;
  const b = isWin ? prefix.toLowerCase() : prefix;
  return a === b || a.startsWith(b.endsWith('/') ? b : b + '/');
}

/** WSL distribution and Linux path of a \\wsl$ / \\wsl.localhost URI. */
export function parseWslUri(uri: vscode.Uri): { distro: string; path: string } | undefined {
  if (uri.scheme !== 'file') {
    return undefined;
  }
  const auth = uri.authority.toLowerCase();
  if (auth !== 'wsl$' && auth !== 'wsl.localhost') {
    return undefined;
  }
  const m = /^\/([^/]+)(\/.*)?$/.exec(uri.path);
  if (!m) {
    return undefined;
  }
  return { distro: m[1], path: m[2] || '/' };
}

export function identityMapper(): Mapper {
  return {
    toServer: (u) => u.toString(),
    toClient: (s) => vscode.Uri.parse(s),
    serverPath: (u) => (u.scheme === 'file' ? u.fsPath : undefined),
    describe: () => 'none (same machine)',
  };
}

export function makeMapper(opts: MapperOptions): Mapper {
  const mappings = opts.mappings
    .filter((m) => m.local && m.remote)
    .map((m) => ({ local: norm(m.local), remote: norm(m.remote) }))
    // Longest prefixes first.
    .sort((a, b) => b.local.length - a.local.length);

  const serverPath = (uri: vscode.Uri): string | undefined => {
    if (uri.scheme === 'dvicode') {
      return uri.path;
    }
    if (uri.scheme !== 'file') {
      return undefined;
    }
    const wsl = parseWslUri(uri);
    if (wsl) {
      return wsl.path;
    }
    const local = norm(uri.fsPath);
    for (const m of mappings) {
      if (startsWithPath(local, m.local)) {
        return m.remote + local.slice(m.local.length);
      }
    }
    if (opts.wslDistro !== undefined) {
      const d = /^([a-zA-Z]):(\/.*)?$/.exec(local);
      if (d) {
        return `/mnt/${d[1].toLowerCase()}${d[2] || ''}`;
      }
    }
    return undefined;
  };

  return {
    serverPath,
    toServer(uri: vscode.Uri): string {
      const p = serverPath(uri);
      if (p === undefined) {
        return uri.toString();
      }
      return vscode.Uri.from({ scheme: 'file', path: p }).toString();
    },
    toClient(s: string): vscode.Uri {
      const uri = vscode.Uri.parse(s);
      if (uri.scheme !== 'file') {
        return uri;
      }
      const p = uri.path;
      for (const m of mappings) {
        if (p === m.remote || p.startsWith(m.remote + '/')) {
          return vscode.Uri.file(m.local + p.slice(m.remote.length));
        }
      }
      if (opts.wslDistro !== undefined) {
        const d = /^\/mnt\/([a-z])(\/.*)?$/.exec(p);
        if (d && isWin) {
          return vscode.Uri.file(`${d[1]}:${d[2] || '/'}`);
        }
        if (!opts.remoteAuthority) {
          return vscode.Uri.from({ scheme: 'file', authority: 'wsl.localhost', path: `/${opts.wslDistro}${p}` });
        }
      }
      if (opts.remoteAuthority) {
        return vscode.Uri.from({ scheme: 'dvicode', authority: opts.remoteAuthority, path: p });
      }
      return uri;
    },
    describe(): string {
      const parts: string[] = [];
      if (opts.wslDistro !== undefined) {
        parts.push(`WSL distribution '${opts.wslDistro}'`);
      }
      if (opts.remoteAuthority) {
        parts.push(`dvicode://${opts.remoteAuthority}`);
      }
      for (const m of mappings) {
        parts.push(`${m.local} -> ${m.remote}`);
      }
      return parts.length ? parts.join('; ') : 'none';
    },
  };
}
