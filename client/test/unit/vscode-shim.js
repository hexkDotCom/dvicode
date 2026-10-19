// The parts of the `vscode` module that pathmap.ts and remotefs.ts use, for unit tests that
// run in plain Node. Uri comes from vscode-uri, which is VS Code's own URI implementation.

const { URI, Utils } = require('vscode-uri');

class Uri extends URI {}
for (const k of ['parse', 'file', 'from', 'revive', 'isUri']) {
  Uri[k] = URI[k].bind(URI);
}
Uri.joinPath = Utils.joinPath;

class Disposable {
  constructor(fn) {
    this.fn = fn;
  }
  dispose() {
    this.fn?.();
  }
}

class EventEmitter {
  constructor() {
    this.listeners = [];
    this.event = (l) => {
      this.listeners.push(l);
      return new Disposable(() => (this.listeners = this.listeners.filter((x) => x !== l)));
    };
  }
  fire(e) {
    for (const l of this.listeners) {
      l(e);
    }
  }
}

class FileSystemError extends Error {
  constructor(code, what) {
    super(`${code}: ${what instanceof URI ? what.toString() : what}`);
    this.code = code;
  }
}
for (const code of ['FileNotFound', 'FileExists', 'FileNotADirectory', 'FileIsADirectory', 'NoPermissions', 'Unavailable']) {
  FileSystemError[code] = (what) => new FileSystemError(code, what);
}

module.exports = {
  Uri,
  Disposable,
  EventEmitter,
  FileSystemError,
  FileType: { Unknown: 0, File: 1, Directory: 2, SymbolicLink: 64 },
  FilePermission: { Readonly: 1 },
  FileChangeType: { Changed: 1, Created: 2, Deleted: 3 },
};
