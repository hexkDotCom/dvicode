// Run before `vsce package`: refuses to package an extension without its engine or the license
// files, which scripts/package.sh and the Release workflow put in place.
import * as fs from 'node:fs';

const required = ['server/linux-x64/dvicode-server', 'LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md', 'dist/terminateProcess.sh'];
const missing = required.filter((f) => !fs.existsSync(f));
if (missing.length) {
  console.error(`cannot package: missing ${missing.join(', ')}; build with scripts/package.sh`);
  process.exit(1);
}
