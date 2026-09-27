#!/usr/bin/env node
/*
 * Release announcement drafts for X, Discord and Reddit, built from the
 * version's section of CHANGELOG.md.
 *
 * Usage
 *   node scripts/release-posts.js                 # version from VERSION
 *   node scripts/release-posts.js 5.35.0
 *   node scripts/release-posts.js --only discord  # one platform
 *   node scripts/release-posts.js --out /tmp/posts   # also write x.txt, discord.md, reddit.md
 *   node scripts/release-posts.js --prerelease | --stable   # override detection
 *   node scripts/release-posts.js --url https://…      # override the release link
 *
 * The changelog section is read as groups: a bold paragraph line
 * (`**One install path for everything.**`) starts a group, and each `- `
 * bullet under it — including its wrapped continuation lines — is one item.
 * An item's bold lead (`- **Stream install.** Longer text…`) is its short form.
 *
 * Pre-release status comes from `gh release view` when gh is available, so
 * the posts say "pre-release" exactly when GitHub does; pass --prerelease or
 * --stable to decide it yourself.
 */
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const REPO = "phantomptr/ps5upload";
const DISCORD_INVITE = "https://discord.gg/fzK3xddtrM";
const DOCKER_IMAGE = "ghcr.io/phantomptr/ps5upload-engine-webui";
const X_LIMIT = 280;
const X_URL_LEN = 23; // X counts every link as 23 characters
const DISCORD_LIMIT = 2000;

const repoRoot = path.resolve(__dirname, "..");
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
};
const positional = args.filter(
  (a, i) => !a.startsWith("--") && !["--url", "--only", "--out"].includes(args[i - 1]),
);

const version =
  positional[0] || fs.readFileSync(path.join(repoRoot, "VERSION"), "utf8").trim();
const tag = `v${version}`;
const releaseUrl = option("--url") || `https://github.com/${REPO}/releases/tag/${tag}`;
const only = option("--only");
const outDir = option("--out");

function detectPrerelease() {
  if (flag("--prerelease")) return true;
  if (flag("--stable")) return false;
  try {
    const out = execFileSync(
      "gh",
      ["release", "view", tag, "--repo", REPO, "--json", "isPrerelease", "--jq", ".isPrerelease"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    );
    return out.trim() === "true";
  } catch {
    return false; // no gh, no release yet: say nothing about it
  }
}
const prerelease = detectPrerelease();

// ── Parse the changelog section ─────────────────────────────────────────────

function readSection() {
  const changelog = fs.readFileSync(path.join(repoRoot, "CHANGELOG.md"), "utf8");
  const lines = changelog.split("\n");
  const start = lines.findIndex((l) => l.trim() === `## ${version}`);
  if (start < 0) {
    console.error(`No "## ${version}" section in CHANGELOG.md`);
    process.exit(1);
  }
  const body = [];
  for (const line of lines.slice(start + 1)) {
    if (line.startsWith("## ") || line.trim() === "---") break;
    body.push(line);
  }
  return body;
}

/** [{ heading, items: [{ lead, text }] }] */
function parseGroups(body) {
  const groups = [];
  let group = null;
  let item = null;
  const ensureGroup = () => {
    if (!group) {
      group = { heading: null, items: [] };
      groups.push(group);
    }
  };
  for (const raw of body) {
    const line = raw.trim();
    if (!line) {
      item = null;
      continue;
    }
    if (line.startsWith("- ")) {
      ensureGroup();
      item = { text: line.slice(2) };
      group.items.push(item);
    } else if (item && raw.startsWith("  ")) {
      item.text += ` ${line}`; // wrapped continuation of the bullet
    } else if (/^\*\*.+\*\*$/.test(line)) {
      group = { heading: line.slice(2, -2), items: [] };
      groups.push(group);
      item = null;
    }
  }
  for (const g of groups) {
    for (const it of g.items) {
      const m = it.text.match(/^\*\*(.+?)\*\*\s*(.*)$/);
      it.lead = m ? m[1] : it.text;
      it.rest = m ? m[2] : "";
    }
  }
  return groups.filter((g) => g.items.length > 0);
}

const stripMd = (s) =>
  s
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");

/** One line for short formats: the bold lead, or the first sentence. */
function shortItem(it) {
  const s = stripMd(it.lead).trim();
  const trim = (t) => t.replace(/[.,:;]$/, "");
  if (s !== stripMd(it.text).trim()) return trim(s);
  return trim(s.split(/(?<=[.!?])\s/)[0]);
}

const groups = parseGroups(readSection());
if (groups.length === 0) {
  console.error(`The ${version} section has no bullet points to announce.`);
  process.exit(1);
}
const headline = groups[0].heading ? stripMd(groups[0].heading).replace(/\.$/, "") : null;
const channel = prerelease ? " (pre-release)" : "";

// ── X ───────────────────────────────────────────────────────────────────────

/** Length as X counts it: every URL is 23 characters. */
const xLength = (s) => s.replace(/https?:\/\/\S+/g, "x".repeat(X_URL_LEN)).length;

function buildX() {
  const lead = `PS5Upload ${tag}${channel} is out!`;
  const main = [lead, headline, releaseUrl].filter(Boolean).join("\n\n");
  // Pack every group heading and item into as few replies as fit, without
  // splitting a group heading from its first item.
  const lines = [];
  for (const g of groups) {
    const items = g.items.map((it) => {
      const line = `• ${shortItem(it)}`;
      return xLength(line) > X_LIMIT ? `${line.slice(0, X_LIMIT - 1)}…` : line;
    });
    // The first heading is already the main post's headline.
    if (g.heading && g !== groups[0]) lines.push({ text: stripMd(g.heading), heading: true });
    for (const text of items) lines.push({ text, heading: false });
  }
  lines.push({ text: `Bugs, questions, ideas: ${DISCORD_INVITE}`, heading: false });
  const replies = [];
  let current = [];
  const fits = (extra) => xLength([...current, ...extra].join("\n")) <= X_LIMIT;
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    // A heading only goes where its first item also fits.
    const block = l.heading && lines[i + 1] ? [l.text, lines[i + 1].text] : [l.text];
    if (current.length > 0 && !fits(block)) {
      replies.push(current.join("\n"));
      current = [];
    }
    current.push(...block);
    if (block.length === 2) i++;
  }
  if (current.length > 0) replies.push(current.join("\n"));
  return { main, replies };
}

// ── Discord ─────────────────────────────────────────────────────────────────

function buildDiscord() {
  const footer = [
    "",
    `**Download:** ${releaseUrl}`,
    `**Docker (web UI):** \`${DOCKER_IMAGE}:${version}\``,
    `**Help & bug reports:** ${DISCORD_INVITE}`,
  ];
  if (prerelease) {
    footer.unshift("", "_Pre-release: in Settings → Updates, turn on “Get pre-release versions” to be offered it._");
  }
  const render = (full) => {
    const out = [`@everyone`, `## PS5Upload ${tag}${channel} is out`];
    for (const g of groups) {
      out.push("");
      if (g.heading) out.push(`**${stripMd(g.heading)}**`);
      for (const it of g.items) {
        out.push(full ? `- ${it.text}` : `- ${shortItem(it)}`);
      }
    }
    return [...out, ...footer].join("\n");
  };
  // Full bullets when they fit Discord's 2000-character limit, short ones when not.
  const full = render(true);
  return full.length <= DISCORD_LIMIT ? full : render(false);
}

// ── Reddit ──────────────────────────────────────────────────────────────────

function buildReddit() {
  const title = `PS5Upload ${tag}${channel} released${headline ? ` — ${headline}` : ""}`;
  const body = [`**PS5Upload ${tag}** is out${prerelease ? " as a pre-release" : ""}.`];
  for (const g of groups) {
    body.push("");
    if (g.heading) body.push(`### ${stripMd(g.heading)}`, "");
    for (const it of g.items) body.push(`- ${it.text}`);
  }
  body.push(
    "",
    `**Download:** ${releaseUrl}`,
    "",
    `**Docker (web UI):** \`${DOCKER_IMAGE}:${version}\``,
    "",
    `**Discord (help, bugs, feature requests):** ${DISCORD_INVITE}`,
  );
  return { title, body: body.join("\n") };
}

// ── Output ──────────────────────────────────────────────────────────────────

const x = buildX();
const discord = buildDiscord();
const reddit = buildReddit();

const sections = {
  x: () => {
    const lines = [`=== X — main post (${xLength(x.main)}/${X_LIMIT}) ===`, x.main];
    x.replies.forEach((r, i) =>
      lines.push("", `--- reply ${i + 1} (${xLength(r)}/${X_LIMIT}) ---`, r),
    );
    return lines.join("\n");
  },
  discord: () => `=== Discord (${discord.length}/${DISCORD_LIMIT}) ===\n${discord}`,
  reddit: () => `=== Reddit ===\nTitle: ${reddit.title}\n\n${reddit.body}`,
};

if (only && !sections[only]) {
  console.error(`--only must be one of: ${Object.keys(sections).join(", ")}`);
  process.exit(2);
}
const chosen = only ? [only] : Object.keys(sections);
console.log(chosen.map((k) => sections[k]()).join("\n\n"));

if (outDir) {
  fs.mkdirSync(outDir, { recursive: true });
  const files = {
    x: ["x.txt", [x.main, ...x.replies].join("\n\n---\n\n")],
    discord: ["discord.md", discord],
    reddit: ["reddit.md", `${reddit.title}\n\n${reddit.body}`],
  };
  for (const k of chosen) {
    const [name, text] = files[k];
    fs.writeFileSync(path.join(outDir, name), `${text}\n`);
  }
  console.error(`\nWrote ${chosen.map((k) => files[k][0]).join(", ")} to ${outDir}`);
}
