#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

const repoRoot = path.join(__dirname, '..');
const nextVersion = process.argv[2];

if (!nextVersion || !/^\d+\.\d+\.\d+$/.test(nextVersion)) {
  console.error('Usage: node scripts/update-version.js <x.y.z>');
  process.exit(1);
}

const writeFile = (relPath, content) => {
  fs.writeFileSync(path.join(repoRoot, relPath), content);
};

const updateJsonVersion = (relPath) => {
  const fullPath = path.join(repoRoot, relPath);
  if (!fs.existsSync(fullPath)) return;
  const json = JSON.parse(fs.readFileSync(fullPath, 'utf8'));
  if (json.version) json.version = nextVersion;
  if (json.packages && json.packages[''] && json.packages[''].version) {
    json.packages[''].version = nextVersion;
  }
  fs.writeFileSync(fullPath, JSON.stringify(json, null, 2) + '\n');
};

const updateText = (relPath, replacers) => {
  const fullPath = path.join(repoRoot, relPath);
  if (!fs.existsSync(fullPath)) return;
  let content = fs.readFileSync(fullPath, 'utf8');
  for (const [pattern, replace] of replacers) {
    content = content.replace(pattern, replace);
  }
  fs.writeFileSync(fullPath, content);
};

// VERSION file
writeFile('VERSION', `${nextVersion}\n`);

// Desktop app version constants
updateText('desktop/electron/main.js', [
  [/const VERSION = '.*?';/, `const VERSION = '${nextVersion}';`],
]);

// package.json + package-lock.json in desktop
updateJsonVersion('desktop/package.json');
updateJsonVersion('desktop/package-lock.json');

// README/FAQ release lines
updateText('README.md', [
  [/Current release: \*\*v\d+\.\d+\.\d+\*\*\./, `Current release: **v${nextVersion}**.`],
]);
updateText('FAQ.md', [
  [/Latest release: \*\*v\d+\.\d+\.\d+\*\*\./, `Latest release: **v${nextVersion}**.`],
]);

console.log(`Version updated to ${nextVersion}`);
