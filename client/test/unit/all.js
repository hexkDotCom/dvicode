// Runs every *.test.js in this folder (Node 20's --test takes no wildcards).
const fs = require('fs');
const path = require('path');

for (const f of fs.readdirSync(__dirname).filter((f) => f.endsWith('.test.js')).sort()) {
  require(path.join(__dirname, f));
}
