#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

const repoRoot = path.resolve(__dirname, '..');
const argVersion = process.argv[2] && !process.argv[2].startsWith('--') ? process.argv[2] : null;
const version = argVersion || fs.readFileSync(path.join(repoRoot, 'VERSION'), 'utf8').trim();
const releaseUrl = process.argv.includes('--url')
  ? process.argv[process.argv.indexOf('--url') + 1]
  : `https://github.com/phantomptr/ps5upload/releases/tag/v${version}`;

const changelog = fs.readFileSync(path.join(repoRoot, 'CHANGELOG.md'), 'utf8');
const sectionRegex = new RegExp(`## \\\[${version}\\\]([\\s\\S]*?)(?=\\n## \\[|$)`);
const match = changelog.match(sectionRegex);
if (!match) {
  console.error(`Could not find changelog section for v${version}`);
  process.exit(1);
}
const section = match[1];
const bullets = [];
for (const line of section.split('\n')) {
  const trimmed = line.trim();
  if (trimmed.startsWith('- ')) {
    bullets.push(trimmed.slice(2));
  }
}

const pick = (count) => bullets.slice(0, count);
const joinBullets = (items, prefix = '• ') => items.map((item) => `${prefix}${item}`).join('\n');

const discordInvite = 'https://discord.gg/fzK3xddtrM';
const xPost = `PS5 Upload v${version} is out! ${releaseUrl}\nDiscord: ${discordInvite}\nHighlights below 👇`;

const buildThread = (items) => {
  const replies = [];
  let current = ['What’s new:'];
  const flush = () => {
    if (current.length > 1) {
      replies.push(current.join('\n'));
    }
    current = ['More:'];
  };

  for (const item of items) {
    const line = `• ${item}`;
    const candidate = [...current, line].join('\n');
    if (candidate.length > 280) {
      flush();
      current = ['More:'];
      if ([...current, line].join('\n').length > 280) {
        replies.push(`• ${item.slice(0, 260)}…`);
        continue;
      }
    }
    current.push(line);
  }

  if (current.length > 1) {
    replies.push(current.join('\n'));
  }

  replies.push(releaseUrl);
  return replies;
};

const replies = buildThread(pick(12));

const discordPost = [
  `@everyone`,
  `## PS5 Upload v${version} is live!`,
  '',
  joinBullets(pick(6)),
  '',
  `**Release:** ${releaseUrl}`,
  `**Discord:** ${discordInvite} (request features / report bugs)`,
  '**Screenshot:** (attach if you want)',
].join('\n');

const redditTitle = `PS5 Upload v${version} released - extraction queue + UI polish`;
const redditPost = [
  `Hi all! **PS5 Upload v${version}** is out.`,
  '',
  '### Highlights',
  '```',
  joinBullets(pick(6), '- '),
  '```',
  '',
  `**Release:** ${releaseUrl}`,
  '',
  `**Discord:** ${discordInvite} (request features / report bugs)`,
  '',
  '**Screenshot:** (optional)',
].join('\n');

console.log('=== X (Twitter) ===');
console.log(xPost);
console.log('\n=== X Replies (Thread) ===');
if (replies.length === 0) {
  console.log('(no extra bullets found)');
} else {
  replies.forEach((reply, idx) => {
    console.log(`Reply ${idx + 1}:\n${reply}\n`);
  });
}
console.log('\n=== Discord ===');
console.log(discordPost);
console.log('\n=== Reddit ===');
console.log(`Title: ${redditTitle}`);
console.log(redditPost);
