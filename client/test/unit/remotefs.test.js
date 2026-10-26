// Unit tests of src/remotefs.ts: dvicode:// files go only to the server that serves their
// authority, and the engine's errors become VS Code file system errors.

const test = require('node:test');
const assert = require('node:assert/strict');
const { load } = require('./load');

const { vscode, remotefs } = load(process.platform);

/** A RemoteFs on a fake language client serving `served`; `reply(method, params)` answers. */
function fsFor(served, reply = () => null) {
  const calls = [];
  const client = {
    sendRequest: async (method, params) => {
      calls.push({ method, params });
      return reply(method, params);
    },
  };
  return { fs: new remotefs.RemoteFs(async () => client, () => served), calls };
}

const uri = (s) => vscode.Uri.parse(s);

async function rejectsWith(p, code) {
  await assert.rejects(p, (e) => {
    assert.equal(e.code, code, e.message);
    return true;
  });
}

test.describe('sameAuthority', () => {
  test.it('compares host names in any case', () => {
    assert.ok(remotefs.sameAuthority('ssh+Build01', 'ssh+build01'));
    assert.ok(remotefs.sameAuthority('wsl+Ubuntu', 'wsl+ubuntu'));
  });
  test.it('treats encoded and plain user@host alike', () => {
    assert.ok(remotefs.sameAuthority('ssh+me@build01', 'ssh+me%40build01'));
    // A login name keeps its case; the host does not.
    assert.ok(remotefs.sameAuthority('ssh+Alice@Build01', 'ssh+Alice%40build01'));
    assert.ok(!remotefs.sameAuthority('ssh+Alice@host', 'ssh+alice@host'));
    assert.ok(remotefs.sameAuthority('tcp+127.0.0.1:7777', 'tcp+127.0.0.1%3A7777'));
  });
  test.it('tells servers apart', () => {
    assert.ok(!remotefs.sameAuthority('ssh+build01', 'ssh+build02'));
    assert.ok(!remotefs.sameAuthority('ssh+build01', 'wsl+build01'));
    assert.ok(!remotefs.sameAuthority('ssh+build01', 'ssh+build01.example.com'));
    // Not valid percent-encoding: compared as written.
    assert.ok(remotefs.sameAuthority('ssh+a%zz', 'SSH+A%ZZ'));
    assert.ok(!remotefs.sameAuthority('ssh+a%zz', 'ssh+a%yy'));
  });
});

test.describe('RemoteFs', () => {
  test.it('reads files of the server it is connected to', async () => {
    const { fs, calls } = fsFor('ssh+build01', (m) => (m === 'dvicode/fs/readFile' ? { content: Buffer.from('module m; endmodule\n').toString('base64') } : null));
    const data = await fs.readFile(uri('dvicode://ssh+build01/home/me/a%20b.sv'));
    assert.equal(Buffer.from(data).toString(), 'module m; endmodule\n');
    assert.deepEqual(calls, [{ method: 'dvicode/fs/readFile', params: { path: '/home/me/a b.sv' } }]);
  });
  test.it('accepts the authority in another spelling', async () => {
    for (const [served, folder] of [
      ['ssh+Build01', 'dvicode://ssh+build01/x'],
      ['ssh+me@build01', 'dvicode://ssh+me%2540build01/x'],
    ]) {
      const { fs, calls } = fsFor(served, () => ({ type: 1, size: 3, mtime: 1, ctime: 1, readonly: false }));
      const u = uri(folder);
      await fs.stat(u);
      assert.equal(calls.length, 1, `${served} serves ${u.authority}`);
    }
  });
  test.it('refuses files of another server, and of a local engine', async () => {
    for (const served of ['ssh+build02', 'wsl+Ubuntu', undefined]) {
      const { fs, calls } = fsFor(served);
      await rejectsWith(fs.readFile(uri('dvicode://ssh+build01/home/me/a.sv')), 'Unavailable');
      await rejectsWith(fs.writeFile(uri('dvicode://ssh+build01/home/me/a.sv'), new Uint8Array(), { create: true, overwrite: true }), 'Unavailable');
      assert.deepEqual(calls, [], `served by ${served}`);
    }
  });
  test.it('checks both ends of a rename', async () => {
    const { fs, calls } = fsFor('ssh+build01');
    await rejectsWith(fs.rename(uri('dvicode://ssh+build01/a.sv'), uri('dvicode://ssh+build02/a.sv'), { overwrite: false }), 'Unavailable');
    await rejectsWith(fs.rename(uri('dvicode://ssh+build02/a.sv'), uri('dvicode://ssh+build01/a.sv'), { overwrite: false }), 'Unavailable');
    assert.deepEqual(calls, []);
    await fs.rename(uri('dvicode://ssh+build01/a.sv'), uri('dvicode://ssh+build01/b.sv'), { overwrite: true });
    assert.deepEqual(calls, [{ method: 'dvicode/fs/rename', params: { from: '/a.sv', to: '/b.sv', overwrite: true } }]);
  });
  test.it('turns engine errors into file system errors', async () => {
    for (const code of ['FileNotFound', 'FileExists', 'NoPermissions', 'FileIsADirectory', 'FileNotADirectory']) {
      const { fs } = fsFor('wsl+Ubuntu', () => Promise.reject({ message: 'no', data: { code } }));
      await rejectsWith(fs.stat(uri('dvicode://wsl+Ubuntu/x')), code);
    }
    const { fs } = fsFor('wsl+Ubuntu', () => Promise.reject(new Error('connection lost')));
    await assert.rejects(fs.readDirectory(uri('dvicode://wsl+Ubuntu/x')), (e) => e.code === 'Unavailable' && /connection lost/.test(e.message));
  });
  test.it('reports read-only files', async () => {
    const { fs } = fsFor('wsl+Ubuntu', () => ({ type: 1, size: 3, mtime: 5, ctime: 4, readonly: true }));
    const s = await fs.stat(uri('dvicode://wsl+Ubuntu/x'));
    assert.deepEqual(s, { type: vscode.FileType.File, size: 3, mtime: 5, ctime: 4, permissions: vscode.FilePermission.Readonly });
  });
  test.it('announces created, changed and deleted files', async () => {
    let created = true;
    const { fs, calls } = fsFor('wsl+Ubuntu', (m) => (m === 'dvicode/fs/writeFile' ? { created } : null));
    const events = [];
    fs.onDidChangeFile((e) => events.push(...e.map((x) => `${x.type}:${x.uri.path}`)));
    const u = uri('dvicode://wsl+Ubuntu/p/a.sv');
    await fs.writeFile(u, Buffer.from('x'), { create: true, overwrite: false });
    created = false;
    await fs.writeFile(u, Buffer.from('y'), { create: false, overwrite: true });
    await fs.delete(u, { recursive: false });
    const { Created, Changed, Deleted } = vscode.FileChangeType;
    assert.deepEqual(events, [`${Created}:/p/a.sv`, `${Changed}:/p/a.sv`, `${Deleted}:/p/a.sv`]);
    assert.deepEqual(calls[0].params, { path: '/p/a.sv', content: Buffer.from('x').toString('base64'), create: true, overwrite: false });
  });
});
