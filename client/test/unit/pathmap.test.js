// Unit tests of src/pathmap.ts: URIs between VS Code and the engine (WSL, SSH, pathMappings,
// dvicode:// folders). Every case runs as if on Windows and on Linux, whatever the host.

const test = require('node:test');
const assert = require('node:assert/strict');
const { load } = require('./load');

const win = load('win32');
const nix = load('linux');

/** Mapper `m` turns client URI `client` into `server`, and `server` back into `client`. */
function both({ vscode }, m, client, server) {
  const c = typeof client === 'string' ? vscode.Uri.parse(client) : client;
  assert.equal(m.toServer(c), server, `toServer(${c})`);
  assert.equal(m.toClient(server).toString(), c.toString(), `toClient(${server})`);
}

test.describe('parseWslUri', () => {
  const { vscode, pathmap } = win;
  test.it('reads wsl.localhost and wsl$ folders, in any case', () => {
    assert.deepEqual(pathmap.parseWslUri(vscode.Uri.parse('file://wsl.localhost/Ubuntu/home/me/p')), { distro: 'Ubuntu', path: '/home/me/p' });
    assert.deepEqual(pathmap.parseWslUri(vscode.Uri.parse('file://wsl%24/Ubuntu-22.04/home/me')), { distro: 'Ubuntu-22.04', path: '/home/me' });
    assert.deepEqual(pathmap.parseWslUri(vscode.Uri.file('\\\\WSL.LOCALHOST\\Debian\\srv')), { distro: 'Debian', path: '/srv' });
  });
  test.it('maps the distribution root to /', () => {
    assert.deepEqual(pathmap.parseWslUri(vscode.Uri.parse('file://wsl.localhost/Ubuntu')), { distro: 'Ubuntu', path: '/' });
    assert.deepEqual(pathmap.parseWslUri(vscode.Uri.parse('file://wsl.localhost/Ubuntu/')), { distro: 'Ubuntu', path: '/' });
  });
  test.it('ignores other hosts and schemes', () => {
    assert.equal(pathmap.parseWslUri(vscode.Uri.parse('file://nas/share/x')), undefined);
    assert.equal(pathmap.parseWslUri(vscode.Uri.parse('file:///d%3A/x')), undefined);
    assert.equal(pathmap.parseWslUri(vscode.Uri.parse('dvicode://wsl.localhost/Ubuntu/x')), undefined);
    assert.equal(pathmap.parseWslUri(vscode.Uri.parse('file://wsl.localhost/')), undefined);
  });
});

test.describe('identityMapper', () => {
  for (const env of [win, nix]) {
    test.it(`passes URIs through (${env === win ? 'Windows' : 'Linux'})`, () => {
      const m = env.pathmap.identityMapper();
      const u = env.vscode.Uri.parse('file:///home/me/a%20b.sv');
      assert.equal(m.toServer(u), u.toString());
      assert.equal(m.toClient('file:///home/me/a%20b.sv').toString(), u.toString());
      assert.equal(m.serverPath(env.vscode.Uri.parse('untitled:Untitled-1')), undefined);
    });
  }
});

test.describe('WSL, Windows drives', () => {
  const { vscode, pathmap } = win;
  const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });

  test.it('maps drive letters to /mnt/<drive> and back', () => {
    both(win, m, vscode.Uri.file('D:\\work\\a.sv'), 'file:///mnt/d/work/a.sv');
    both(win, m, 'file:///c%3A/Users/me/x.svh', 'file:///mnt/c/Users/me/x.svh');
    assert.equal(m.toServer(vscode.Uri.parse('file:///D:/work/a.sv')), 'file:///mnt/d/work/a.sv');
    assert.equal(m.serverPath(vscode.Uri.file('E:\\a.sv')), '/mnt/e/a.sv');
  });
  test.it('maps drive roots', () => {
    assert.equal(m.toServer(vscode.Uri.file('D:\\')), 'file:///mnt/d');
    const root = m.toClient('file:///mnt/d');
    assert.equal(root.fsPath, 'd:\\');
    assert.equal(m.toClient('file:///mnt/d/').fsPath, 'd:\\');
  });
  test.it('opens other Linux files through \\\\wsl.localhost', () => {
    const u = m.toClient('file:///home/me/p/a.sv');
    assert.equal(u.scheme, 'file');
    assert.equal(u.authority, 'wsl.localhost');
    assert.equal(u.path, '/Ubuntu/home/me/p/a.sv');
    assert.equal(u.fsPath, '\\\\wsl.localhost\\Ubuntu\\home\\me\\p\\a.sv');
    // /mnt/wsl is not a drive.
    assert.equal(m.toClient('file:///mnt/wsl/x').path, '/Ubuntu/mnt/wsl/x');
    assert.equal(m.toClient('file:///mnt/data/x').path, '/Ubuntu/mnt/data/x');
  });
  test.it('leaves other URIs alone', () => {
    assert.equal(m.toServer(vscode.Uri.parse('untitled:Untitled-1')), 'untitled:Untitled-1');
    assert.equal(m.toServer(vscode.Uri.file('\\\\nas\\share\\a.sv')), 'file://nas/share/a.sv');
    assert.equal(m.toClient('untitled:Untitled-1').toString(), 'untitled:Untitled-1');
  });
  test.it('without Windows, /mnt paths stay in the distribution', () => {
    const n = nix.pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });
    assert.equal(n.toClient('file:///mnt/d/x').toString(), 'file://wsl.localhost/Ubuntu/mnt/d/x');
  });
});

test.describe('WSL, \\\\wsl$ and \\\\wsl.localhost folders', () => {
  const { vscode, pathmap } = win;
  for (const host of ['wsl.localhost', 'wsl$']) {
    test.it(`keeps the ${host} authority in replies`, () => {
      const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', wslHost: host, mappings: [] });
      const local = vscode.Uri.from({ scheme: 'file', authority: host, path: '/Ubuntu/home/me/p/a.sv' });
      both(win, m, local, 'file:///home/me/p/a.sv');
      assert.equal(m.toClient('file:///home/me/p/a.sv').authority, host);
      assert.equal(m.toServer(vscode.Uri.file(`\\\\${host}\\Ubuntu\\home\\me\\p\\b.sv`)), 'file:///home/me/p/b.sv');
      // The distribution root.
      assert.equal(m.toServer(vscode.Uri.from({ scheme: 'file', authority: host, path: '/Ubuntu' })), 'file:///');
      // A folder under /mnt opened through this host keeps its spelling, not the drive's.
      both(win, m, vscode.Uri.from({ scheme: 'file', authority: host, path: '/Ubuntu/mnt/c/proj/a.sv' }), 'file:///mnt/c/proj/a.sv');
    });
  }
  test.it('defaults to wsl.localhost', () => {
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });
    assert.equal(m.toServer(vscode.Uri.parse('file://wsl%24/Ubuntu/home/a.sv')), 'file:///home/a.sv');
    assert.equal(m.toClient('file:///home/a.sv').authority, 'wsl.localhost');
  });
  test.it('matches the distribution name in any case', () => {
    const m = pathmap.makeMapper({ wslDistro: 'ubuntu', mappings: [] });
    assert.equal(m.toServer(vscode.Uri.parse('file://wsl.localhost/Ubuntu/home/a.sv')), 'file:///home/a.sv');
  });
  test.it('does not map files of another distribution', () => {
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });
    const other = vscode.Uri.parse('file://wsl.localhost/Debian/home/a.sv');
    assert.equal(m.serverPath(other), undefined);
    assert.equal(m.toServer(other), other.toString());
  });
  test.it('does not map WSL folders for an engine outside WSL', () => {
    // A Windows engine (local mode) with pathMappings reads \\wsl.localhost paths itself.
    const m = pathmap.makeMapper({ mappings: [{ local: 'Z:\\', remote: '/home/me' }] });
    const u = vscode.Uri.parse('file://wsl.localhost/Ubuntu/home/a.sv');
    assert.equal(m.toServer(u), u.toString());
  });
});

test.describe('pathMappings', () => {
  test.it('maps both ways and picks the longest prefix on each side (Windows)', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({
      mappings: [
        { local: 'Z:\\', remote: '/home/me' },
        { local: 'Y:\\', remote: '/home/me/data' },
        { local: 'Z:\\proj\\lib\\', remote: '/srv/lib' },
      ],
    });
    both(win, m, vscode.Uri.file('Z:\\proj\\a.sv'), 'file:///home/me/proj/a.sv');
    both(win, m, vscode.Uri.file('Z:\\proj\\lib\\b.sv'), 'file:///srv/lib/b.sv');
    both(win, m, vscode.Uri.file('Y:\\c.sv'), 'file:///home/me/data/c.sv');
    // Z:\data is /home/me/data on the server, whose files the editor sees as Y:\.
    assert.equal(m.toServer(vscode.Uri.file('Z:\\data\\c.sv')), 'file:///home/me/data/c.sv');
    assert.equal(m.toClient('file:///home/me/data').fsPath, 'y:\\');
    // Whole path components only.
    assert.equal(m.toServer(vscode.Uri.file('Z:\\proj\\library\\x.sv')), 'file:///home/me/proj/library/x.sv');
    assert.equal(m.toClient('file:///home/meg/x.sv').toString(), 'file:///home/meg/x.sv');
    assert.equal(m.toClient('file:///srv/library/x.sv').toString(), 'file:///srv/library/x.sv');
  });
  test.it('ignores the case of local paths on Windows only', () => {
    // A Windows engine: its file URIs have the drive after a `/`, on any client.
    for (const { vscode, pathmap } of [win, nix]) {
      const m = pathmap.makeMapper({ mappings: [{ local: vscode === win.vscode ? 'Z:\\proj' : '/z/proj', remote: 'C:\\srv\\proj' }] });
      const back = m.toClient('file:///c%3A/srv/Proj/a.sv');
      assert.equal(back.fsPath.replace(/\\/g, '/').toLowerCase(), (vscode === win.vscode ? 'z:/proj/a.sv' : '/z/proj/a.sv'), back.toString());
      assert.equal(m.toServer(back).toLowerCase(), 'file:///c%3a/srv/proj/a.sv');
    }
    const w = win.pathmap.makeMapper({ mappings: [{ local: 'Z:\\Proj', remote: '/home/me/proj' }] });
    assert.equal(w.toServer(win.vscode.Uri.file('z:\\PROJ\\Rtl\\a.sv')), 'file:///home/me/proj/Rtl/a.sv');
    // Server paths are case-sensitive everywhere.
    assert.equal(w.toClient('file:///home/me/Proj/a.sv').toString(), 'file:///home/me/Proj/a.sv');

    const n = nix.pathmap.makeMapper({ mappings: [{ local: '/Volumes/Proj', remote: '/home/me/proj' }] });
    assert.equal(n.toServer(nix.vscode.Uri.file('/Volumes/Proj/a.sv')), 'file:///home/me/proj/a.sv');
    assert.equal(n.toServer(nix.vscode.Uri.file('/volumes/proj/a.sv')), 'file:///volumes/proj/a.sv');
    assert.equal(n.toClient('file:///home/me/proj/a.sv').toString(), 'file:///Volumes/Proj/a.sv');
    assert.equal(n.toClient('file:///home/me/PROJ/a.sv').toString(), 'file:///home/me/PROJ/a.sv');
  });
  test.it('accepts forward slashes and trailing slashes on Windows', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ mappings: [{ local: 'z:/proj/', remote: '/home/me/proj/' }] });
    both(win, m, vscode.Uri.file('Z:\\proj\\a.sv'), 'file:///home/me/proj/a.sv');
    assert.equal(m.toServer(vscode.Uri.file('Z:\\proj')), 'file:///home/me/proj');
    assert.equal(m.toClient('file:///home/me/proj').fsPath, 'z:\\proj');
  });
  test.it('maps UNC shares', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ mappings: [{ local: '\\\\nas\\proj', remote: '/proj' }] });
    both(win, m, vscode.Uri.file('\\\\nas\\proj\\a.sv'), 'file:///proj/a.sv');
  });
  test.it('keeps backslashes in Linux file names', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [{ local: '/data', remote: '/srv/data' }] });
    both(nix, m, vscode.Uri.file('/data/a\\b.sv'), 'file:///srv/data/a%5Cb.sv');
  });
  test.it('skips incomplete entries', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [{ local: '', remote: '/x' }, { local: '/y', remote: '' }] });
    assert.equal(m.toServer(vscode.Uri.file('/y/a.sv')), 'file:///y/a.sv');
    assert.equal(m.toClient('file:///x/a.sv').toString(), 'file:///x/a.sv');
    assert.equal(m.describe(), 'none');
  });
  test.it('keeps WSL drive mapping for paths outside the mappings', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [{ local: 'D:\\big', remote: '/home/me/big' }] });
    both(win, m, vscode.Uri.file('D:\\big\\a.sv'), 'file:///home/me/big/a.sv');
    both(win, m, vscode.Uri.file('D:\\small\\a.sv'), 'file:///mnt/d/small/a.sv');
  });
});

test.describe('root mappings', () => {
  test.it('local / (Linux and macOS clients)', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [{ local: '/', remote: '/net/ws1' }] });
    both(nix, m, vscode.Uri.file('/home/me/a.sv'), 'file:///net/ws1/home/me/a.sv');
    both(nix, m, vscode.Uri.file('/'), 'file:///net/ws1');
    assert.equal(m.toClient('file:///opt/x').toString(), 'file:///opt/x');
  });
  test.it('remote / (Linux and macOS clients)', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [{ local: '/mnt/server', remote: '/' }] });
    both(nix, m, vscode.Uri.file('/mnt/server/home/a.sv'), 'file:///home/a.sv');
    both(nix, m, vscode.Uri.file('/mnt/server'), 'file:///');
    assert.equal(m.toServer(vscode.Uri.file('/mnt/serverx/a.sv')), 'file:///mnt/serverx/a.sv');
  });
  test.it('remote / on a Windows drive', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ mappings: [{ local: 'Z:\\', remote: '/' }] });
    both(win, m, vscode.Uri.file('Z:\\proj\\a.sv'), 'file:///proj/a.sv');
    assert.equal(m.toServer(vscode.Uri.file('Z:\\')), 'file:///');
    const root = m.toClient('file:///');
    assert.equal(root.fsPath, 'z:\\');
    assert.equal(root.toString(), 'file:///z%3A/');
  });
  test.it('/ on both sides', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [{ local: '/', remote: '/' }] });
    both(nix, m, vscode.Uri.file('/home/a.sv'), 'file:///home/a.sv');
    both(nix, m, vscode.Uri.file('/'), 'file:///');
  });
});

test.describe('dvicode:// folders', () => {
  for (const env of [win, nix]) {
    const { vscode, pathmap } = env;
    const os = env === win ? 'Windows' : 'Linux';
    test.it(`server files stay dvicode:// files (${os})`, () => {
      const m = pathmap.makeMapper({ remoteAuthority: 'ssh+build01', mappings: [] });
      both(env, m, 'dvicode://ssh+build01/home/me/p/a.sv', 'file:///home/me/p/a.sv');
      both(env, m, 'dvicode://ssh+build01/opt/uvm/src/uvm_pkg.sv', 'file:///opt/uvm/src/uvm_pkg.sv');
      const u = m.toClient('file:///home/me/p/a.sv');
      assert.equal(u.scheme, 'dvicode');
      assert.equal(u.authority, 'ssh+build01');
      // Path mappings too: the replies name the documents of the workspace.
      const mapped = pathmap.makeMapper({ remoteAuthority: 'ssh+build01', mappings: [{ local: env === win ? 'C:/shared' : '/shared', remote: '/proj' }] });
      both(env, mapped, 'dvicode://ssh+build01/proj/a.sv', 'file:///proj/a.sv');
    });
    test.it(`a WSL folder keeps /mnt paths on dvicode:// (${os})`, () => {
      const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', remoteAuthority: 'wsl+Ubuntu', mappings: [] });
      both(env, m, 'dvicode://wsl+Ubuntu/mnt/d/work/a.sv', 'file:///mnt/d/work/a.sv');
      both(env, m, 'dvicode://wsl+Ubuntu/home/me/a.sv', 'file:///home/me/a.sv');
      assert.equal(m.toClient('file:///mnt/c').toString(), 'dvicode://wsl%2Bubuntu/mnt/c');
    });
    test.it(`a document of another server is not this server's file (${os})`, () => {
      const m = pathmap.makeMapper({ remoteAuthority: 'ssh+build02', mappings: [] });
      const u = vscode.Uri.parse('dvicode://ssh+build01/home/me/a.sv');
      assert.equal(m.serverPath(u), undefined);
      assert.equal(m.toServer(u), u.toString());
      assert.equal(pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] }).serverPath(vscode.Uri.parse('dvicode://wsl+ubuntu/home/me/a.sv')), '/home/me/a.sv');
      assert.equal(pathmap.identityMapper().serverPath(u), undefined);
    });
    test.it(`keeps an encoded authority (${os})`, () => {
      // "Open Folder on the Server" encodes user@host.
      const m = pathmap.makeMapper({ remoteAuthority: 'ssh+me%40build01', mappings: [] });
      const u = vscode.Uri.from({ scheme: 'dvicode', authority: 'ssh+me%40build01', path: '/home/me/a.sv' });
      both(env, m, u, 'file:///home/me/a.sv');
      assert.equal(m.toClient('file:///home/me/a.sv').authority, 'ssh+me%40build01');
    });
  }
});

test.describe('SSH: server-only files', () => {
  test.it('paths outside the local folders become dvicode://ssh+host', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [], fallbackAuthority: 'ssh+build01', localRoots: ['/home/me/proj/'] });
    both(nix, m, vscode.Uri.file('/home/me/proj/rtl/a.sv'), 'file:///home/me/proj/rtl/a.sv');
    both(nix, m, vscode.Uri.file('/home/me/proj'), 'file:///home/me/proj');
    both(nix, m, 'dvicode://ssh+build01/opt/uvm/src/uvm_pkg.sv', 'file:///opt/uvm/src/uvm_pkg.sv');
    assert.equal(m.toClient('file:///home/me/project2/a.sv').scheme, 'dvicode');
    assert.equal(m.toClient('file:///home/me/Proj/a.sv').scheme, 'dvicode');
    assert.match(m.describe(), /dvicode:\/\/ssh\+build01/);
  });
  test.it('mapped paths stay files, others go to the server (Windows client)', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({
      mappings: [{ local: 'D:\\proj', remote: '/home/me/proj' }],
      fallbackAuthority: 'ssh+me@build01',
      localRoots: ['d:\\proj'],
    });
    both(win, m, vscode.Uri.file('D:\\proj\\a.sv'), 'file:///home/me/proj/a.sv');
    const u = m.toClient('file:///opt/uvm/x.sv');
    assert.equal(u.scheme, 'dvicode');
    assert.equal(u.authority, 'ssh+me@build01');
    assert.equal(u.path, '/opt/uvm/x.sv');
    assert.equal(m.toServer(u), 'file:///opt/uvm/x.sv');
  });
  test.it('without local folders every server file is remote', () => {
    const { pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [], fallbackAuthority: 'ssh+build01', localRoots: [] });
    assert.equal(m.toClient('file:///home/me/a.sv').toString(), 'dvicode://ssh%2Bbuild01/home/me/a.sv');
  });
});

test.describe('spaces, non-ASCII and percent-encoding', () => {
  test.it('WSL drive paths', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });
    const u = vscode.Uri.file('D:\\My Work\\Über ñ\\a#b%c&d.sv');
    const s = m.toServer(u);
    assert.equal(s, 'file:///mnt/d/My%20Work/%C3%9Cber%20%C3%B1/a%23b%25c%26d.sv');
    assert.equal(m.toClient(s).fsPath, u.fsPath);
    // The engine's own spelling of the same URI.
    assert.equal(m.toClient('file:///mnt/d/My%20Work/%c3%9cber%20%c3%b1/a%23b%25c%26d.sv').fsPath, u.fsPath);
  });
  test.it('WSL folders', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', mappings: [] });
    const u = vscode.Uri.file('\\\\wsl.localhost\\Ubuntu\\home\\me\\日本 語\\a.sv');
    both(win, m, u, 'file:///home/me/%E6%97%A5%E6%9C%AC%20%E8%AA%9E/a.sv');
  });
  test.it('pathMappings', () => {
    const { vscode, pathmap } = win;
    const m = pathmap.makeMapper({ mappings: [{ local: 'Z:\\My Proj', remote: '/home/me/my proj' }] });
    both(win, m, vscode.Uri.file('Z:\\My Proj\\é.sv'), 'file:///home/me/my%20proj/%C3%A9.sv');
  });
  test.it('dvicode:// files', () => {
    const { vscode, pathmap } = nix;
    const m = pathmap.makeMapper({ mappings: [], fallbackAuthority: 'ssh+build01', localRoots: ['/home/me/a b'] });
    both(nix, m, vscode.Uri.file('/home/me/a b/c d.sv'), 'file:///home/me/a%20b/c%20d.sv');
    const r = m.toClient('file:///opt/x%20y/%C3%A9.sv');
    assert.equal(r.scheme, 'dvicode');
    assert.equal(r.path, '/opt/x y/é.sv');
    assert.equal(m.toServer(r), 'file:///opt/x%20y/%C3%A9.sv');
  });
});

test.describe('Engine addresses', () => {
  const { pathmap } = win;
  test.it('knows this machine by every spelling', () => {
    for (const h of ['', 'localhost', '127.0.0.1', '127.0.0.2', '[::1]', '[0:0:0:0:0:0:0:1]', '[::ffff:127.0.0.1]']) {
      assert.ok(pathmap.isLoopback(h), h);
    }
    for (const h of ['build01', '10.0.0.5', '[::2]', '[fe80::1]']) {
      assert.ok(!pathmap.isLoopback(h), h);
    }
  });
  test.it('reads host:port, [v6]:port and a bare port', () => {
    assert.deepEqual(pathmap.tcpEndpoint('7777'), { host: '127.0.0.1', port: 7777 });
    assert.deepEqual(pathmap.tcpEndpoint('127.0.0.1:7777'), { host: '127.0.0.1', port: 7777 });
    assert.deepEqual(pathmap.tcpEndpoint('Build01:9000'), { host: 'build01', port: 9000 });
    assert.deepEqual(pathmap.tcpEndpoint('[::1]:7777'), { host: '::1', port: 7777 });
  });
});

test.describe('WSL folders opened through \\wsl.localhost', () => {
  const { vscode, pathmap } = win;
  test.it('keep their spelling under /mnt; other drive files stay drive paths', () => {
    const m = pathmap.makeMapper({ wslDistro: 'Ubuntu', wslHost: 'wsl.localhost', wslRoots: ['/mnt/c/proj'], mappings: [] });
    assert.equal(m.toClient('file:///mnt/c/proj/a.sv').toString(), vscode.Uri.from({ scheme: 'file', authority: 'wsl.localhost', path: '/Ubuntu/mnt/c/proj/a.sv' }).toString());
    assert.equal(m.toClient('file:///mnt/c/other/b.sv').toString(), vscode.Uri.file('c:/other/b.sv').toString());
  });
});
