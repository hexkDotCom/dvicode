// End-to-end test: launches the installed VS Code with this extension in development mode
// against a workspace and runs test/suite.js inside it.
//
//   node test/runTest.js <folder-path-or-uri> [label]
//
// Uses an isolated user-data and extensions directory, with other extensions disabled.

const path = require('path');
const fs = require('fs');
const os = require('os');
const { runTests } = require('@vscode/test-electron');

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
  return candidates.find((c) => c && fs.existsSync(c));
}

async function main() {
  const target = process.argv[2];
  const label = process.argv[3] || 'e2e';
  if (!target) {
    console.error('usage: node test/runTest.js <folder-path-or-uri> [label]');
    process.exit(2);
  }
  const code = findCode();
  if (!code) {
    console.error('VS Code not found; set VSCODE_EXECUTABLE');
    process.exit(2);
  }
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'dvicode-e2e-'));
  const result = path.join(tmp, 'result.json');
  fs.mkdirSync(path.join(tmp, 'user', 'User'), { recursive: true });
  fs.writeFileSync(
    path.join(tmp, 'user', 'User', 'settings.json'),
    JSON.stringify({
      'security.allowedUNCHosts': ['wsl.localhost', 'wsl$'],
      'security.workspace.trust.enabled': false,
      // Extra settings for a run, e.g. SSH options: DVICODE_E2E_SETTINGS='{"dvicode.server.sshArgs": [...]}'
      ...JSON.parse(process.env.DVICODE_E2E_SETTINGS || '{}'),
    }),
  );
  // URIs (\wsl.localhost, dvicode://) go through a .code-workspace file: the test runner
  // starts VS Code through a shell on Windows, which mangles such arguments.
  let folderArgs = [target];
  if (/^[a-z][a-z0-9+.-]+:\/\//i.test(target)) {
    const ws = path.join(tmp, 'e2e.code-workspace');
    fs.writeFileSync(ws, JSON.stringify({ folders: [{ uri: target }] }));
    folderArgs = [ws];
  }
  try {
    await runTests({
      vscodeExecutablePath: code,
      extensionDevelopmentPath: path.resolve(__dirname, '..'),
      extensionTestsPath: path.resolve(__dirname, 'suite.js'),
      launchArgs: [
        ...folderArgs,
        '--disable-extensions',
        '--disable-workspace-trust',
        '--skip-welcome',
        '--skip-release-notes',
        '--user-data-dir', path.join(tmp, 'user'),
        '--extensions-dir', path.join(tmp, 'ext'),
      ],
      extensionTestsEnv: { DVICODE_E2E_RESULT: result, DVICODE_E2E_LABEL: label },
    });
  } catch (e) {
    console.error(`[${label}] VS Code test run failed: ${e}`);
  }
  if (fs.existsSync(result)) {
    const r = JSON.parse(fs.readFileSync(result, 'utf8'));
    for (const t of r.tests) {
      console.log(`[${label}] ${t.ok ? 'PASS' : 'FAIL'} ${t.name}${t.detail ? ' - ' + t.detail : ''}`);
    }
    console.log(`[${label}] ${r.tests.filter((t) => t.ok).length}/${r.tests.length} passed; server: ${r.target}`);
    process.exit(r.tests.every((t) => t.ok) ? 0 : 1);
  }
  console.error(`[${label}] no result file`);
  process.exit(1);
}

main();
