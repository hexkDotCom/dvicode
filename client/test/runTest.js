// End-to-end test: launches the installed VS Code with this extension in development mode
// against a workspace and runs test/suite.js inside it.
//
//   node test/runTest.js <folder-path-or-uri> [label]
//
// Uses an isolated user-data and extensions directory, with other extensions disabled. Without
// an installed VS Code (VSCODE_EXECUTABLE or a usual location), stable VS Code is downloaded
// into .vscode-test. Exit codes: 0 all passed, 1 a test failed, 3 VS Code reported no result.
//
// Remote windows (VS Code's own Remote - WSL / Remote - SSH): with DVICODE_E2E_REMOTE set to
// the window's authority, `wsl+<distro>` or `ssh-remote+<host>`, the folder is a path on that
// machine, and the extension and the suite are copied there (~/.dvicode-e2e) and run in the
// remote extension host. The window needs Microsoft's resolver extension (Remote - WSL or
// Remote - SSH), the only extension in its extensions directory: .vscode-test/remote-extensions,
// where it is installed on first use. For SSH, DVICODE_E2E_SSH_CONFIG is an ssh config file that
// defines <host> (port, user, key); both VS Code and this script connect through it.

const path = require('path');
const fs = require('fs');
const os = require('os');
const cp = require('child_process');
const { downloadAndUnzipVSCode, runTests, resolveCliArgsFromVSCodeExecutablePath } = require('@vscode/test-electron');

const RESOLVERS = { wsl: 'ms-vscode-remote.remote-wsl', 'ssh-remote': 'ms-vscode-remote.remote-ssh' };

function findCode() {
  if (process.env.VSCODE_EXECUTABLE) {
    return process.env.VSCODE_EXECUTABLE;
  }
  const candidates = [
    path.join(process.env.LOCALAPPDATA || '', 'Programs', 'Microsoft VS Code', 'Code.exe'),
    'C:\\Program Files\\Microsoft VS Code\\Code.exe',
    '/Applications/Visual Studio Code.app/Contents/MacOS/Electron',
    '/usr/share/code/code',
  ];
  const found = candidates.find((c) => c && fs.existsSync(macBinary(c)));
  return found && macBinary(found);
}

/**
 * Newer VS Code names its macOS program Contents/MacOS/Code; @vscode/test-electron still points
 * at Contents/MacOS/Electron.
 */
function macBinary(p) {
  const code = p.replace(/\/Contents\/MacOS\/Electron$/, '/Contents/MacOS/Code');
  return code !== p && !fs.existsSync(p) && fs.existsSync(code) ? code : p;
}

/** Runs a shell script on the remote machine of a remote window; resolves with its stdout. */
function remoteSh(authority, script, input) {
  const [kind, target] = [authority.slice(0, authority.indexOf('+')), authority.slice(authority.indexOf('+') + 1)];
  let cmd;
  let args;
  if (kind === 'wsl') {
    [cmd, args] = ['wsl.exe', ['-d', target, '-e', 'sh', '-c', script]];
  } else if (kind === 'ssh-remote') {
    const config = process.env.DVICODE_E2E_SSH_CONFIG ? ['-F', process.env.DVICODE_E2E_SSH_CONFIG] : [];
    // On Windows the OpenSSH client that Remote - SSH uses too, not one from Git or MSYS.
    const winSsh = path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'OpenSSH', 'ssh.exe');
    cmd = process.platform === 'win32' && fs.existsSync(winSsh) ? winSsh : 'ssh';
    args = [...config, '-T', '-o', 'BatchMode=yes', target, script];
  } else {
    throw new Error(`unsupported DVICODE_E2E_REMOTE '${authority}' (wsl+<distro> or ssh-remote+<host>)`);
  }
  return new Promise((resolve, reject) => {
    const p = cp.spawn(cmd, args, { stdio: [input ? 'pipe' : 'ignore', 'pipe', 'pipe'], windowsHide: true });
    let out = '';
    let err = '';
    p.stdout.on('data', (d) => (out += d));
    p.stderr.on('data', (d) => (err += d));
    p.on('error', reject);
    p.on('close', (code) => (code === 0 ? resolve(out) : reject(new Error(`${cmd} ${target}: exit ${code}: ${err.trim()}`))));
    if (input) {
      input.pipe(p.stdin);
    }
  });
}

/** An extensions directory holding only the resolver for `authority`, installed if missing. */
function resolverExtensions(code, authority) {
  const id = RESOLVERS[authority.slice(0, authority.indexOf('+'))];
  // Under the client folder: WSL does not see every folder of the Windows profile.
  const dir = path.resolve(__dirname, '..', '.vscode-test', 'remote-extensions');
  if (!fs.existsSync(dir) || !fs.readdirSync(dir).some((e) => e.startsWith(`${id}-`))) {
    const [cli, ...args] = resolveCliArgsFromVSCodeExecutablePath(code);
    // code.cmd needs a shell on Windows, which needs the paths quoted.
    const win = process.platform === 'win32';
    const q = (s) => (win ? `"${s}"` : s);
    const r = cp.spawnSync(q(cli), [...args, '--extensions-dir', q(dir), '--install-extension', id], { stdio: 'inherit', shell: win });
    if (r.status !== 0) {
      throw new Error(`installing ${id} failed (exit ${r.status})`);
    }
  }
  return dir;
}

/**
 * Copies the extension (what a package contains: manifest, bundle, engines, grammars) and the
 * suite to ~/.dvicode-e2e on the remote machine; returns that directory.
 */
async function stageRemote(authority, extDir, tmp) {
  const stage = path.join(tmp, 'stage');
  for (const e of fs.readdirSync(extDir)) {
    if (!['node_modules', 'src', 'test', '.vscode-test'].includes(e) && !e.endsWith('.vsix')) {
      fs.cpSync(path.join(extDir, e), path.join(stage, e), { recursive: true });
    }
  }
  fs.mkdirSync(path.join(stage, 'test'), { recursive: true });
  fs.copyFileSync(path.join(__dirname, 'suite.js'), path.join(stage, 'test', 'suite.js'));
  const home = (await remoteSh(authority, 'echo "$HOME"')).trim();
  const dir = `${home}/.dvicode-e2e/extension`;
  // Windows' own tar (bsdtar), not one from Git or MSYS that takes C: for a host name.
  const tarExe = process.platform === 'win32' ? path.join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'tar.exe') : 'tar';
  const tar = cp.spawn(tarExe, ['-cf', '-', '-C', stage, '.'], { stdio: ['ignore', 'pipe', 'inherit'], windowsHide: true });
  await remoteSh(authority, `rm -rf '${dir}' && mkdir -p '${dir}' && tar -C '${dir}' -xf -`, tar.stdout);
  return dir;
}

async function main() {
  const target = process.argv[2];
  const label = process.argv[3] || 'e2e';
  if (!target) {
    console.error('usage: node test/runTest.js <folder-path-or-uri> [label]');
    process.exit(2);
  }
  const remote = process.env.DVICODE_E2E_REMOTE;
  let code = findCode();
  if (!code) {
    console.log(`[${label}] no VS Code installed (VSCODE_EXECUTABLE): downloading stable VS Code`);
  }
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'dvicode-e2e-'));
  let result = path.join(tmp, 'result.json');
  // DVICODE_E2E_USER_DATA: a profile kept from run to run, so an editor that asks to sign in
  // (Cursor) remembers it. Its settings are written again each run.
  const userData = process.env.DVICODE_E2E_USER_DATA || path.join(tmp, 'user');
  fs.mkdirSync(path.join(userData, 'User'), { recursive: true });
  fs.writeFileSync(
    path.join(userData, 'User', 'settings.json'),
    JSON.stringify({
      'security.allowedUNCHosts': ['wsl.localhost', 'wsl$'],
      'security.workspace.trust.enabled': false,
      'extensions.autoUpdate': false,
      'extensions.autoCheckUpdates': false,
      // Remote - SSH: no questions about the host's platform, no login terminal.
      ...(remote && process.env.DVICODE_E2E_SSH_CONFIG
        ? {
            'remote.SSH.configFile': process.env.DVICODE_E2E_SSH_CONFIG,
            'remote.SSH.remotePlatform': { [remote.replace(/^ssh-remote\+/, '')]: 'linux' },
            'remote.SSH.showLoginTerminal': false,
          }
        : {}),
      // Extra settings for a run, e.g. SSH options: DVICODE_E2E_SETTINGS='{"dvicode.server.sshArgs": [...]}'
      ...JSON.parse(process.env.DVICODE_E2E_SETTINGS || '{}'),
    }),
  );
  // DVICODE_E2E_EXTENSION: an unpacked VSIX (its extension/ folder) instead of the sources.
  let extensionDevelopmentPath = process.env.DVICODE_E2E_EXTENSION || path.resolve(__dirname, '..');
  let extensionTestsPath = path.resolve(__dirname, 'suite.js');
  let extensionsDir = path.join(tmp, 'ext');
  let folderUri = /^[a-z][a-z0-9+.-]+:\/\//i.test(target) ? target : undefined;
  let remoteDir;
  if (remote) {
    remoteDir = await stageRemote(remote, extensionDevelopmentPath, tmp);
    extensionDevelopmentPath = `vscode-remote://${remote}${remoteDir}`;
    extensionTestsPath = `vscode-remote://${remote}${remoteDir}/test/suite.js`;
    code = code || (await downloadAndUnzipVSCode());
    extensionsDir = resolverExtensions(code, remote);
    folderUri = `vscode-remote://${remote}${target}`;
    // The suite runs on the remote machine and writes its result next to itself.
    result = path.join(tmp, 'remote-result.json');
  }
  // URIs (\wsl.localhost, dvicode://, vscode-remote://) go through a .code-workspace file: the
  // test runner starts VS Code through a shell on Windows, which mangles such arguments.
  let folderArgs = [target];
  if (folderUri) {
    const ws = path.join(tmp, 'e2e.code-workspace');
    fs.writeFileSync(ws, JSON.stringify({ folders: [{ uri: folderUri }], ...(remote ? { remoteAuthority: remote } : {}) }));
    folderArgs = [ws];
  }
  // Downloaded here rather than by runTests, whose own path to the macOS program is outdated.
  code = macBinary(code || (await downloadAndUnzipVSCode()));
  // VS Code failing (a crash on shutdown, say) fails the run even if the tests reported success.
  let runFailed = false;
  try {
    await runTests({
      vscodeExecutablePath: code,
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: [
        ...folderArgs,
        // A remote window needs its resolver extension, the only one in its extensions directory.
        ...(remote ? [] : ['--disable-extensions']),
        '--disable-workspace-trust',
        '--skip-welcome',
        '--skip-release-notes',
        '--user-data-dir', userData,
        '--extensions-dir', extensionsDir,
      ],
      extensionTestsEnv: remote ? {} : { DVICODE_E2E_RESULT: result },
    });
  } catch (e) {
    runFailed = true;
    console.error(`[${label}] VS Code test run failed: ${e}`);
  }
  if (remote) {
    try {
      fs.writeFileSync(result, await remoteSh(remote, `cat '${remoteDir}/test/result.json'`));
    } catch (e) {
      console.error(`[${label}] no result on the remote machine: ${e.message}`);
    }
  }
  if (fs.existsSync(result)) {
    const r = JSON.parse(fs.readFileSync(result, 'utf8'));
    for (const t of r.tests) {
      console.log(`[${label}] ${t.ok ? 'PASS' : 'FAIL'} ${t.name}${t.detail ? ' - ' + t.detail : ''}`);
    }
    // The extension host the suite ran in: "local" or the remote window's kind, and its platform.
    console.log(`[${label}] extension host: ${r.host || '?'}`);
    // A remote window runs the extension in the remote extension host, on Linux.
    const wantHost = remote && `${remote.slice(0, remote.indexOf('+'))} linux-`;
    const hostOk = !wantHost || String(r.host).startsWith(wantHost);
    if (!hostOk) {
      console.error(`[${label}] the extension ran in the '${r.host}' extension host, not '${wantHost}...'`);
    }
    console.log(`[${label}] ${r.tests.filter((t) => t.ok).length}/${r.tests.length} passed; server: ${r.target}`);
    process.exit(r.tests.every((t) => t.ok) && hostOk && !runFailed ? 0 : 1);
  }
  console.error(`[${label}] no result file`);
  process.exit(3);
}

main().catch((e) => {
  // Setting up the run failed (copying to the remote machine, say): not worth a retry.
  console.error(`${e && e.message ? e.message : e}`);
  process.exit(1);
});
