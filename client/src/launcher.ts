// Decides where the language server runs and how to reach it.
//
// The server always speaks LSP over the stdio of the process we start, so `wsl.exe` and
// `ssh` carry the connection without any port or certificate. The `tcp` mode attaches to a
// server that was started separately with `dvicode-server --listen`.

import * as cp from 'child_process';
import * as crypto from 'crypto';
import * as fs from 'fs';
import * as net from 'net';
import * as path from 'path';
import * as vscode from 'vscode';
import { Executable, ServerOptions, StreamInfo } from 'vscode-languageclient/node';
import { identityMapper, makeMapper, Mapper, parseWslUri, PathMapping } from './pathmap';

export type Mode = 'local' | 'wsl' | 'ssh' | 'tcp' | 'custom';

export interface Launch {
  mode: Mode;
  serverOptions: ServerOptions;
  mapper: Mapper;
  /** Human-readable target, e.g. "WSL Ubuntu" or "ssh build01". */
  target: string;
  /** The dvicode:// authority whose files this server can read and write (undefined: this machine). */
  authority?: string;
}

interface Cfg {
  mode: string;
  path: string;
  autoDeploy: boolean;
  wslDistro: string;
  sshHost: string;
  sshArgs: string[];
  sshCommand: string;
  launchCommand: string;
  tcpAddress: string;
  tcpToken: string;
  logLevel: string;
  extraArgs: string[];
  pathMappings: PathMapping[];
}

function readCfg(): Cfg {
  const c = vscode.workspace.getConfiguration('dvicode');
  return {
    mode: c.get('server.mode', 'auto'),
    path: c.get('server.path', ''),
    autoDeploy: c.get('server.autoDeploy', true),
    wslDistro: c.get('server.wslDistro', ''),
    sshHost: c.get('server.sshHost', ''),
    sshArgs: c.get('server.sshArgs', []),
    sshCommand: c.get('server.sshCommand', 'ssh') || 'ssh',
    launchCommand: c.get('server.launchCommand', '${SERVER_CMD}'),
    tcpAddress: c.get('server.tcpAddress', '127.0.0.1:7777'),
    tcpToken: c.get('server.tcpToken', ''),
    logLevel: c.get('server.logLevel', 'info'),
    extraArgs: c.get('server.extraArgs', []),
    pathMappings: c.get('pathMappings', []),
  };
}

/** `dvicode://wsl+Ubuntu/...` or `dvicode://ssh+host/...` folders say where they live. */
export function parseRemoteAuthority(auth: string): { kind: 'wsl' | 'ssh' | 'tcp'; target: string } | undefined {
  const m = /^(wsl|ssh|tcp)\+(.+)$/.exec(auth);
  if (!m) {
    return undefined;
  }
  return { kind: m[1] as 'wsl' | 'ssh' | 'tcp', target: decodeURIComponent(m[2]) };
}

function bundledServer(ctx: vscode.ExtensionContext, platform: string, arch: string): string {
  const exe = platform === 'win32' ? 'dvicode-server.exe' : 'dvicode-server';
  return ctx.asAbsolutePath(path.join('server', `${platform}-${arch}`, exe));
}

function serverArgs(cfg: Cfg): string[] {
  return ['--stdio', '--log-level', cfg.logLevel, ...cfg.extraArgs];
}

function shQuote(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

function run(cmd: string, args: string[], input?: Buffer, timeoutMs = 60000): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const p = cp.spawn(cmd, args, { windowsHide: true });
    let stdout = '';
    let stderr = '';
    const t = setTimeout(() => p.kill(), timeoutMs);
    p.stdout.on('data', (d) => (stdout += d.toString()));
    p.stderr.on('data', (d) => (stderr += d.toString()));
    p.on('error', (e) => {
      clearTimeout(t);
      resolve({ code: -1, stdout, stderr: stderr + String(e) });
    });
    p.on('close', (code) => {
      clearTimeout(t);
      resolve({ code: code ?? -1, stdout, stderr });
    });
    if (input) {
      p.stdin.end(input);
    } else {
      p.stdin.end();
    }
  });
}

/** wsl.exe prints UTF-16 for its own messages; strip NULs. */
function clean(s: string): string {
  return s.replace(/\u0000/g, '').trim();
}

async function defaultWslDistro(): Promise<string> {
  const r = await run('wsl.exe', ['-e', 'sh', '-c', 'echo $WSL_DISTRO_NAME']);
  const d = clean(r.stdout);
  if (r.code !== 0 || !d) {
    throw new Error(`cannot query the default WSL distribution: ${clean(r.stderr) || 'is WSL installed?'}`);
  }
  return d;
}

/** Version plus a hash of the bundled Linux binary, so a rebuilt server is always redeployed. */
function serverVersion(ctx: vscode.ExtensionContext): string {
  const v = String(ctx.extension.packageJSON.version);
  const bin = bundledServer(ctx, 'linux', 'x64');
  try {
    const h = crypto.createHash('sha256').update(fs.readFileSync(bin)).digest('hex').slice(0, 10);
    return `${v}-${h}`;
  } catch {
    return v;
  }
}

async function prepareWsl(ctx: vscode.ExtensionContext, cfg: Cfg, distro: string, out: vscode.OutputChannel): Promise<string> {
  if (cfg.path) {
    return cfg.path;
  }
  const ver = serverVersion(ctx);
  const target = `$HOME/.dvicode/server/${ver}/dvicode-server`;
  const check = await run('wsl.exe', ['-d', distro, '-e', 'sh', '-c', `test -x "${target}" && test -O "${target}" && echo ok`]);
  if (clean(check.stdout) === 'ok') {
    return target;
  }
  if (!cfg.autoDeploy) {
    throw new Error(`dvicode-server is not installed in WSL '${distro}' (${target}) and dvicode.server.autoDeploy is off`);
  }
  const src = bundledServer(ctx, 'linux', 'x64');
  if (!fs.existsSync(src)) {
    throw new Error(`the extension has no bundled Linux server (${src}); set dvicode.server.path`);
  }
  out.appendLine(`Installing dvicode-server ${ver} into WSL '${distro}'...`);
  const script = `set -e; umask 077; d="$HOME/.dvicode/server/${ver}"; mkdir -p "$d"; cp "$(wslpath -u "$1")" "$d/dvicode-server.tmp.$$"; chmod +x "$d/dvicode-server.tmp.$$"; mv "$d/dvicode-server.tmp.$$" "$d/dvicode-server"; echo ok`;
  const r = await run('wsl.exe', ['-d', distro, '-e', 'sh', '-c', script, 'sh', src]);
  if (clean(r.stdout) !== 'ok') {
    throw new Error(`could not install the server into WSL '${distro}': ${clean(r.stderr)}`);
  }
  return target;
}

function sshBase(cfg: Cfg): string[] {
  return [...cfg.sshArgs, '-T', '-o', 'BatchMode=yes', '-o', 'ServerAliveInterval=30', '-o', 'ConnectTimeout=20'];
}

async function prepareSsh(ctx: vscode.ExtensionContext, cfg: Cfg, host: string, out: vscode.OutputChannel): Promise<string> {
  if (cfg.path) {
    return cfg.path;
  }
  const ver = serverVersion(ctx);
  const target = `$HOME/.dvicode/server/${ver}/dvicode-server`;
  const check = await run(cfg.sshCommand, [...sshBase(cfg), host, `test -x "${target}" && test -O "${target}" && echo ok || uname -sm`]);
  if (check.code === 255 || check.code === -1) {
    throw new Error(`ssh ${host} failed: ${check.stderr.trim()} (DVICode needs key or agent authentication; BatchMode is on)`);
  }
  if (check.stdout.trim() === 'ok') {
    return target;
  }
  const uname = check.stdout.trim();
  if (!/Linux x86_64/.test(uname)) {
    throw new Error(`the bundled server is for Linux x86_64, but ${host} is '${uname}'; build dvicode-server there and set dvicode.server.path`);
  }
  if (!cfg.autoDeploy) {
    throw new Error(`dvicode-server is not installed on ${host} (${target}) and dvicode.server.autoDeploy is off`);
  }
  const src = bundledServer(ctx, 'linux', 'x64');
  out.appendLine(`Installing dvicode-server ${ver} on ${host}...`);
  const script = `set -e; umask 077; d="$HOME/.dvicode/server/${ver}"; mkdir -p "$d"; cat > "$d/dvicode-server.tmp.$$"; chmod +x "$d/dvicode-server.tmp.$$"; mv "$d/dvicode-server.tmp.$$" "$d/dvicode-server"; echo ok`;
  const r = await run(cfg.sshCommand, [...sshBase(cfg), host, `sh -c ${shQuote(script)}`], fs.readFileSync(src), 300000);
  if (r.stdout.trim() !== 'ok') {
    throw new Error(`could not install the server on ${host}: ${r.stderr.trim()}`);
  }
  return target;
}

function tcpStream(address: string, token: string): () => Promise<StreamInfo> {
  return () =>
    new Promise((resolve, reject) => {
      const i = address.lastIndexOf(':');
      const host = i > 0 ? address.slice(0, i) : '127.0.0.1';
      const port = Number(i >= 0 ? address.slice(i + 1) : address);
      const socket = net.connect({ host, port }, () => {
        if (token) {
          socket.write(`DVICODE-TOKEN ${token}\n`);
        }
        resolve({ reader: socket, writer: socket });
      });
      socket.on('error', reject);
    });
}

/** Works out the launch configuration for the current workspace and settings. */
export async function makeLaunch(ctx: vscode.ExtensionContext, out: vscode.OutputChannel): Promise<Launch> {
  const cfg = readCfg();
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri;
  let mode = cfg.mode as Mode | 'auto';
  let wslDistro = cfg.wslDistro;
  let sshHost = cfg.sshHost;
  let tcpAddress = cfg.tcpAddress;
  let remoteAuthority: string | undefined;

  // A dvicode:// folder decides for itself.
  if (folder?.scheme === 'dvicode') {
    const ra = parseRemoteAuthority(folder.authority);
    if (ra) {
      remoteAuthority = folder.authority;
      mode = ra.kind;
      if (ra.kind === 'wsl') {
        wslDistro = ra.target;
      } else if (ra.kind === 'ssh') {
        sshHost = ra.target;
      } else {
        tcpAddress = ra.target;
      }
    }
  }
  const wslFolder = folder ? parseWslUri(folder) : undefined;
  if (mode === 'auto') {
    const localBin = bundledServer(ctx, process.platform, process.arch);
    // An explicit dvicode.server.path that exists here is a local engine, bundled or not.
    const localPath = !!cfg.path && fs.existsSync(cfg.path);
    if (process.platform === 'win32' && (wslFolder || (!fs.existsSync(localBin) && !localPath))) {
      mode = 'wsl';
    } else if (fs.existsSync(localBin) || cfg.path) {
      mode = 'local';
    } else if (sshHost) {
      mode = 'ssh';
    } else {
      throw new Error(`no dvicode-server for ${process.platform}-${process.arch}; set dvicode.server.mode to ssh/tcp/custom or dvicode.server.path`);
    }
  }
  if (mode === 'wsl' && !wslDistro) {
    wslDistro = wslFolder?.distro || (await defaultWslDistro());
  }

  switch (mode) {
    case 'local': {
      const bin = cfg.path || bundledServer(ctx, process.platform, process.arch);
      if (!cfg.path && process.platform !== 'win32') {
        // Archives do not always keep the execute bit.
        try {
          fs.chmodSync(bin, 0o755);
        } catch {
          // Reported when the process fails to start.
        }
      }
      const exe: Executable = { command: bin, args: serverArgs(cfg) };
      return { mode, serverOptions: exe, mapper: makeMapperFor(cfg, undefined, remoteAuthority), target: 'this machine' };
    }
    case 'wsl': {
      if (process.platform !== 'win32') {
        throw new Error('the wsl mode needs Windows');
      }
      const serverPath = await prepareWsl(ctx, cfg, wslDistro, out);
      const args = serverArgs(cfg).map(shQuote).join(' ');
      const exe: Executable = {
        command: 'wsl.exe',
        // A login shell gives the server the user's environment (UVM_HOME, ...).
        args: ['-d', wslDistro, '-e', 'bash', '-lc', `exec "${serverPath}" ${args}`],
      };
      return {
        mode,
        serverOptions: exe,
        mapper: makeMapperFor(cfg, wslDistro, remoteAuthority, wslFolder ? folder?.authority : undefined),
        target: `WSL ${wslDistro}`,
        authority: remoteAuthority ?? `wsl+${wslDistro}`,
      };
    }
    case 'ssh': {
      if (!sshHost) {
        throw new Error('set dvicode.server.sshHost for the ssh mode');
      }
      const serverPath = await prepareSsh(ctx, cfg, sshHost, out);
      const args = serverArgs(cfg).map(shQuote).join(' ');
      const exe: Executable = {
        command: cfg.sshCommand,
        args: [...sshBase(cfg), sshHost, `bash -lc ${shQuote(`exec "${serverPath}" ${args}`)}`],
      };
      const authority = remoteAuthority ?? `ssh+${sshHost}`;
      // Server files outside dvicode.pathMappings and outside the (shared) workspace folders,
      // such as UVM under /opt, open as dvicode:// files.
      const mapper = remoteAuthority
        ? makeMapperFor(cfg, undefined, remoteAuthority)
        : makeMapper({
            mappings: cfg.pathMappings,
            fallbackAuthority: authority,
            localRoots: (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === 'file').map((f) => f.uri.fsPath),
          });
      return { mode, serverOptions: exe, mapper, target: `ssh ${sshHost}`, authority };
    }
    case 'tcp':
      return {
        mode,
        serverOptions: tcpStream(tcpAddress, cfg.tcpToken),
        mapper: makeMapperFor(cfg, undefined, remoteAuthority),
        target: `tcp ${tcpAddress}`,
        authority: remoteAuthority ?? `tcp+${tcpAddress}`,
      };
    case 'custom': {
      const serverCmd = [cfg.path || 'dvicode-server', ...serverArgs(cfg)].map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(' ');
      const cmd = (cfg.launchCommand || '${SERVER_CMD}').replace('${SERVER_CMD}', serverCmd);
      const exe: Executable = { command: cmd, options: { shell: true } };
      return { mode, serverOptions: exe, mapper: makeMapperFor(cfg, undefined, remoteAuthority), target: `custom: ${cmd}` };
    }
  }
  throw new Error(`unknown dvicode.server.mode '${mode}'`);
}

function makeMapperFor(cfg: Cfg, wslDistro: string | undefined, remoteAuthority: string | undefined, wslHost?: string): Mapper {
  if (wslDistro === undefined && !remoteAuthority && cfg.pathMappings.length === 0) {
    return identityMapper();
  }
  return makeMapper({ wslDistro, remoteAuthority, wslHost, mappings: cfg.pathMappings });
}
