import * as esbuild from 'esbuild';
import * as fs from 'node:fs';

const production = process.argv.includes('--production');

// vscode-languageclient kills a stuck server's process tree with this script, which it looks
// for next to its code: next to the bundle once bundled.
fs.mkdirSync('dist', { recursive: true });
fs.copyFileSync('node_modules/vscode-languageclient/lib/node/terminateProcess.sh', 'dist/terminateProcess.sh');
fs.chmodSync('dist/terminateProcess.sh', 0o755);
const watch = process.argv.includes('--watch');

const ctx = await esbuild.context({
  entryPoints: ['src/extension.ts'],
  bundle: true,
  format: 'cjs',
  minify: production,
  sourcemap: !production,
  platform: 'node',
  target: 'node18',
  outfile: 'dist/extension.js',
  external: ['vscode'],
  logLevel: 'info',
});
if (watch) {
  await ctx.watch();
} else {
  await ctx.rebuild();
  await ctx.dispose();
}
