// Loads the compiled client modules (out/unit, see tsconfig.unit.json) with `vscode` replaced
// by vscode-shim.js, as if on the given platform: pathmap.ts and vscode-uri read
// process.platform when they load, so every platform's behaviour is tested on every host.

const Module = require('module');
const path = require('path');

const shim = path.join(__dirname, 'vscode-shim.js');
const out = path.join(__dirname, '..', '..', 'out', 'unit');

const resolve = Module._resolveFilename;
Module._resolveFilename = function (request, ...rest) {
  return request === 'vscode' ? shim : resolve.call(this, request, ...rest);
};

/** @returns {{ vscode: any, pathmap: any, remotefs: any }} */
function load(platform) {
  const real = Object.getOwnPropertyDescriptor(process, 'platform');
  for (const k of Object.keys(require.cache)) {
    if (k.startsWith(out) || k === shim || k.includes(`${path.sep}vscode-uri${path.sep}`)) {
      delete require.cache[k];
    }
  }
  Object.defineProperty(process, 'platform', { ...real, value: platform });
  try {
    return {
      vscode: require(shim),
      pathmap: require(path.join(out, 'pathmap.js')),
      remotefs: require(path.join(out, 'remotefs.js')),
    };
  } finally {
    Object.defineProperty(process, 'platform', real);
  }
}

module.exports = { load };
