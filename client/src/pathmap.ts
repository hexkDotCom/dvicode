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
  /** `wsl$` or `wsl.localhost`, as the workspace folder spells it (default `wsl.localhost`). */
  wslHost?: string;
  /**
   * Authority of dvicode:// URIs for server paths outside `localRoots` (SSH mode: UVM under
   * /opt on the server, say). Paths inside them are shared with this machine and stay files.
   */
  fallbackAuthority?: string;
  localRoots?: string[];
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

/** Forward slashes (backslashes separate only on Windows), no trailing slash. */
function norm(p: string): string {
  let s = isWin ? p.replace(/\\/g, '/') : p;
  if (s.length > 1 && s.endsWith('/')) {
    s = s.slice(0, -1);
  }
  return s;
}

function startsWithPath(p: string, prefix: string, ignoreCase = isWin): boolean {
  const a = ignoreCase ? p.toLowerCase() : p;
  const b = ignoreCase ? prefix.toLowerCase() : prefix;
  return a === b || a.startsWith(b.endsWith('/') ? b : b + '/');
}

/** `prefix` followed by the rest of a path after it, with exactly one `/` between. */
function joinPath(prefix: string, rest: string): string {
  const r = rest.replace(/^\/+/, '');
  const p = prefix.replace(/\/+$/, '');
  if (r) {
    return `${p}/${r}`;
  }
  // `Z:` alone is the current directory of drive Z.
  return /^[a-zA-Z]:$/.test(p) ? `${p}/` : prefix;
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
  // The other direction picks the longest server prefix.
  const reverse = [...mappings].sort((a, b) => b.remote.length - a.remote.length);

  const serverPath = (uri: vscode.Uri): string | undefined => {
    if (uri.scheme === 'dvicode') {
      return uri.path;
    }
    if (uri.scheme !== 'file') {
      return undefined;
    }
    // Only an engine in that same distribution sees a \\wsl$ file under its Linux path.
    const wsl = opts.wslDistro !== undefined ? parseWslUri(uri) : undefined;
    if (wsl) {
      return wsl.distro.toLowerCase() === opts.wslDistro!.toLowerCase() ? wsl.path : undefined;
    }
    const local = norm(uri.fsPath);
    for (const m of mappings) {
      if (startsWithPath(local, m.local)) {
        return joinPath(m.remote, local.slice(m.local.length));
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
      // In a dvicode:// workspace every server file stays a dvicode:// file, so replies name
      // the same documents the editor has open (path mappings included).
      if (opts.remoteAuthority) {
        return vscode.Uri.from({ scheme: 'dvicode', authority: opts.remoteAuthority, path: p });
      }
      for (const m of reverse) {
        if (startsWithPath(p, m.remote, false)) {
          return vscode.Uri.file(joinPath(m.local, p.slice(m.remote.length)));
        }
      }
      if (opts.wslDistro !== undefined) {
        const d = /^\/mnt\/([a-z])(\/.*)?$/.exec(p);
        if (d && isWin) {
          return vscode.Uri.file(`${d[1]}:${d[2] || '/'}`);
        }
        return vscode.Uri.from({ scheme: 'file', authority: opts.wslHost || 'wsl.localhost', path: `/${opts.wslDistro}${p}` });
      }
      if (opts.fallbackAuthority && !(opts.localRoots ?? []).some((r) => startsWithPath(p, norm(r)))) {
        return vscode.Uri.from({ scheme: 'dvicode', authority: opts.fallbackAuthority, path: p });
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
      if (opts.fallbackAuthority) {
        parts.push(`server-only files: dvicode://${opts.fallbackAuthority}`);
      }
      for (const m of mappings) {
        parts.push(`${m.local} -> ${m.remote}`);
      }
      return parts.length ? parts.join('; ') : 'none';
    },
  };
}
