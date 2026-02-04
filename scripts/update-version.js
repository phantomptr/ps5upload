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
  if (!fs.existsSync(fullPath)) return false;
  const json = JSON.parse(fs.readFileSync(fullPath, 'utf8'));
  let changed = false;
  if (json.version) json.version = nextVersion;
  if (json.version === nextVersion) changed = true;
  if (json.packages && json.packages[''] && json.packages[''].version) {
    json.packages[''].version = nextVersion;
    changed = true;
  }
  fs.writeFileSync(fullPath, JSON.stringify(json, null, 2) + '\n');
  return changed;
};

const updateText = (relPath, replacers) => {
  const fullPath = path.join(repoRoot, relPath);
  if (!fs.existsSync(fullPath)) return false;
  let content = fs.readFileSync(fullPath, 'utf8');
  let changed = false;
  for (const [pattern, replace] of replacers) {
    const next = content.replace(pattern, replace);
    if (next !== content) changed = true;
    content = next;
  }
  fs.writeFileSync(fullPath, content);
  return changed;
};

// VERSION file
writeFile('VERSION', `${nextVersion}\n`);

// Desktop app version constants
const updatedDesktopMain = updateText('desktop/electron/main.js', [
  [/const VERSION = '.*?';/, `const VERSION = '${nextVersion}';`],
]);

// package.json + package-lock.json
const updatedDesktopPkg = updateJsonVersion('desktop/package.json');
const updatedDesktopLock = updateJsonVersion('desktop/package-lock.json');
const updatedAppPkg = updateJsonVersion('app/package.json');
const updatedAppLock = updateJsonVersion('app/package-lock.json');

// README/FAQ release lines
const updatedReadme = updateText('README.md', [
  [/Current release: \*\*v\d+\.\d+\.\d+\*\*\./, `Current release: **v${nextVersion}**.`],
]);
const updatedFaq = updateText('FAQ.md', [
  [/Latest release: \*\*v\d+\.\d+\.\d+\*\*\./, `Latest release: **v${nextVersion}**.`],
]);
const updatedAppWorkflow = updateText('.github/workflows/app-package.yml', [
  [/Version tag for naming \(example: v\d+\.\d+\.\d+\)/, `Version tag for naming (example: v${nextVersion})`],
]);

console.log(`Version updated to ${nextVersion}`);
console.log('Updated files:');
[
  ['VERSION', true],
  ['desktop/electron/main.js', updatedDesktopMain],
  ['desktop/package.json', updatedDesktopPkg],
  ['desktop/package-lock.json', updatedDesktopLock],
  ['app/package.json', updatedAppPkg],
  ['app/package-lock.json', updatedAppLock],
  ['README.md', updatedReadme],
  ['FAQ.md', updatedFaq],
  ['.github/workflows/app-package.yml', updatedAppWorkflow],
].forEach(([file, changed]) => {
  console.log(`- ${file}: ${changed ? 'updated' : 'unchanged/missing'}`);
});
