// Translates URIs between VS Code (this machine) and the language server (possibly WSL or a
// remote Linux host).
//
//  VS Code side                                   server side
//  file://wsl.localhost/Ubuntu/home/me/p/a.sv  <->  file:///home/me/p/a.sv     (WSL UNC path)
//  file:///d%3A/work/a.sv                      <->  file:///mnt/d/work/a.sv     (WSL, Windows drive)
//  dvicode://ssh+build01/home/me/p/a.sv        <->  file:///home/me/p/a.sv     (remote folder)
//  file:///z%3A/proj/a.sv (pathMappings)       <->  file:///home/me/proj/a.sv   (Samba/NFS/SSHFS)

import * as net from 'net';
import * as vscode from 'vscode';
import { sameAuthority } from './remotefs';

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
   * Linux paths of the workspace folders opened through `wslHost`: files under /mnt inside
   * them keep that spelling; other /mnt files are drive paths (`C:\...`). Every /mnt path when
   * not given.
   */
  wslRoots?: string[];
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

/** A server path of a mapping: a Windows engine's (`C:\\srv\\proj`) with `/`, whatever this
 * client's platform. */
function normRemote(p: string): string {
  const s = /^[A-Za-z]:[\\/]/.test(p) ? p.replace(/\\/g, '/') : p;
  return s.length > 1 && s.endsWith('/') ? s.slice(0, -1) : s;
}

export function makeMapper(opts: MapperOptions): Mapper {
  const mappings = opts.mappings
    .filter((m) => m.local && m.remote)
    .map((m) => ({ local: norm(m.local), remote: normRemote(m.remote) }))
    // Longest prefixes first.
    .sort((a, b) => b.local.length - a.local.length);
  // The other direction picks the longest server prefix.
  const reverse = [...mappings].sort((a, b) => b.remote.length - a.remote.length);

  // The dvicode:// authorities this server serves: a document of another server (open from an
  // earlier engine) is none of its files.
  const served = [opts.remoteAuthority, opts.fallbackAuthority, opts.wslDistro !== undefined ? `wsl+${opts.wslDistro}` : undefined].filter((a): a is string => !!a);
  const serverPath = (uri: vscode.Uri): string | undefined => {
    if (uri.scheme === 'dvicode') {
      return served.some((a) => sameAuthority(a, uri.authority)) ? uri.path : undefined;
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
      // A Windows engine's file URI has its drive after a `/` (`/C:/srv/a.sv`); its paths
      // ignore case.
      const q = /^\/[A-Za-z]:\//.test(p) ? p.slice(1) : p;
      for (const m of reverse) {
        if (startsWithPath(q, m.remote, /^[A-Za-z]:/.test(m.remote))) {
          return vscode.Uri.file(joinPath(m.local, q.slice(m.remote.length)));
        }
      }
      if (opts.wslDistro !== undefined) {
        const d = /^\/mnt\/([a-z])(\/.*)?$/.exec(p);
        // Under /mnt as a drive path, unless the workspace is opened through \\wsl.localhost:
        // replies must name the documents the editor has open.
        const unc = opts.wslHost !== undefined && (opts.wslRoots === undefined || opts.wslRoots.some((r) => startsWithPath(p, r, false)));
        if (d && isWin && !unc) {
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

/** Host and port of `host:port`, `[v6]:port` or a bare port (on 127.0.0.1). */
export function tcpEndpoint(address: string): { host: string; port: number } {
  const i = address.lastIndexOf(':');
  // `[::1]:7777`: the brackets only delimit an IPv6 address.
  const host = i > 0 ? address.slice(0, i).replace(/^\[(.*)\]$/, '$1') : '127.0.0.1';
  return { host: host.toLowerCase(), port: Number(i >= 0 ? address.slice(i + 1) : address) };
}

/** Whether a host is this machine: localhost, 127.0.0.0/8, ::1 (also as ::ffff:127.x.x.x). */
export function isLoopback(host: string): boolean {
  const h = host.replace(/^\[(.*)\]$/, '$1').toLowerCase();
  if (h === '' || h === 'localhost') {
    return true;
  }
  const loop = new net.BlockList();
  loop.addSubnet('127.0.0.0', 8, 'ipv4');
  loop.addAddress('::1', 'ipv6');
  loop.addSubnet('::ffff:127.0.0.0', 104, 'ipv6');
  if (net.isIPv4(h)) {
    return loop.check(h, 'ipv4');
  }
  return net.isIPv6(h) && loop.check(h, 'ipv6');
}
