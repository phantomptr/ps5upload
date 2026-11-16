#!/usr/bin/env node
/*
 * Release announcement drafts for X, Discord and Reddit, built from the
 * version's section of CHANGELOG.md.
 *
 * Usage
 *   make release-post                             # = node scripts/release-posts.js
 *   node scripts/release-posts.js                 # version from VERSION
 *   node scripts/release-posts.js 5.35.0
 *   node scripts/release-posts.js --only discord  # print one platform
 *   node scripts/release-posts.js --out /tmp/posts   # write there instead
 *   node scripts/release-posts.js --prerelease | --stable   # override detection
 *   node scripts/release-posts.js --url https://…      # override the release link
 *
 * Writes one file per thing you paste to `.release_posts/v<version>/` (git-ignored), shaped
 * for each platform:
 *   X        plain text, no markdown; 280 characters with every link counted as 23. A short
 *            main post with the link (X shows the release as a card), then a reply thread.
 *            One file per post: x-1-main.txt, x-2-reply.txt, …
 *   Discord  markdown: ## heading, bullets, **bold**, `-#` small print, [named links](<url>)
 *            (the <> stops Discord from embedding each link). 2000 characters at most.
 *   Reddit   the title and the body are separate boxes: reddit-title.txt, reddit-body.md
 *            (markdown, ### headings, named links).
 * plus README.md: what to paste where, with each post's length.
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
const { captureSite, copyImages } = require("./post-images.js");

const REPO = "phantomptr/ps5upload";
const DISCORD_INVITE = "https://discord.gg/fzK3xddtrM";
const ISSUES_URL = `https://github.com/${REPO}/issues`;
const COFFEE_URL = "https://ko-fi.com/B0B81S0WUA"; // client/src/lib/supportLinks.ts
const SITE_URL = "https://ps5upload.phantomptr.com/"; // site/ on GitHub Pages
const AUTHOR_URL = "https://phantomptr.com/";
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
const outDir = option("--out") || path.join(repoRoot, ".release_posts", tag);

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

/** Shortens `s` to at most `max` characters: at the last comma that leaves at least half of
 *  it (a list reads fine cut there), else at a word boundary with "…". */
function clip(s, max) {
  if (s.length <= max) return s;
  const cut = s.slice(0, max - 1);
  const comma = cut.lastIndexOf(", ");
  if (comma >= max / 2) return cut.slice(0, comma);
  return `${cut.slice(0, Math.max(cut.lastIndexOf(" "), max / 2)).replace(/[,;:\s]+$/, "")}…`;
}

/** One line for short formats. A bold lead that is a sentence ("**Faster installs.** …")
 *  stands alone; a label ("**Convert:** AMPR LZ4 packs, …") keeps the first clause after it
 *  so the line still says something. Otherwise the first sentence. */
function shortItem(it, max = 64) {
  const lead = stripMd(it.lead).trim();
  const rest = stripMd(it.rest || "").trim();
  if (lead !== stripMd(it.text).trim()) {
    // A label that is already a phrase ("Collection scans show their progress:") stands alone;
    // a short one ("Convert:") needs the clause after it to say anything.
    if (/:$/.test(lead) && rest && lead.split(/\s+/).length < 4) {
      const clause = rest.split(/[;(]|,\s(?=and |or )|\.\s/)[0].trim().replace(/[.,;:]$/, "");
      return clip(`${lead} ${clause}`, max);
    }
    return clip(lead.replace(/[.,:;]$/, ""), max);
  }
  return clip(lead.split(/(?<=[.!?])\s/)[0].replace(/[.,:;]$/, ""), max);
}

const sectionLines = readSection();
const groups = parseGroups(sectionLines);
/** The section's opening paragraph (the line before the bullets), if there is one. */
const summary = (() => {
  const para = [];
  for (const l of sectionLines) {
    if (l.trim().startsWith("- ") || /^\*\*.+\*\*$/.test(l.trim()) || l.startsWith("#")) break;
    if (l.trim()) para.push(l.trim());
    else if (para.length) break;
  }
  return para.length ? stripMd(para.join(" ")) : null;
})();
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
  const lead = `PS5Upload ${tag}${channel} is out 🎮`;
  // The changelog's opening line is the best one-sentence summary there is.
  const intro = summary ? clip(summary, 160) : headline;
  // Without a summary to open with, the post points at the thread below it instead.
  const main = [lead, intro, `Download: ${SITE_URL}`, "#PS5 #PS5Homebrew", intro ? null : "What's new 👇"]
    .filter(Boolean)
    .join("\n\n");
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
  lines.push({ text: `Full notes: ${releaseUrl}\nHelp & ideas: ${DISCORD_INVITE}\nBugs: ${ISSUES_URL}\n☕ ${COFFEE_URL}`, heading: false });
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
  const links = [
    `**[Download ${tag}](<${SITE_URL}>)**`,
    `[Release notes](<${releaseUrl}>)`,
    `[Report a bug](<${ISSUES_URL}>)`,
    `Docker web UI: \`${DOCKER_IMAGE}:${version}\``,
    `☕ [Buy me a coffee](<${COFFEE_URL}>)`,
  ].join(" · ");
  const render = (full, ping) => {
    const out = [];
    if (ping) out.push("@everyone");
    out.push(`## 🎮 PS5Upload ${tag}${channel} is out`);
    if (summary) out.push(summary);
    for (const g of groups) {
      out.push("");
      if (g.heading) out.push(`### ${stripMd(g.heading)}`);
      for (const it of g.items) {
        if (full) {
          out.push(`- ${it.text}`);
          continue;
        }
        // Bold only the label: "**Convert:** AMPR LZ4 asset packs…".
        const line = shortItem(it);
        const lead = stripMd(it.lead).trim();
        const head = /:$/.test(lead) ? lead : null;
        out.push(head && line.startsWith(head) ? `- **${head}**${line.slice(head.length)}` : `- **${line}**`);
      }
    }
    out.push("", links);
    if (prerelease) {
      out.push("-# Pre-release: in Settings → Updates, turn on “Get pre-release versions” to be offered it.");
    }
    out.push("-# Questions and ideas: ask here.");
    return out.join("\n");
  };
  // Full bullets when they fit Discord's 2000-character limit, short ones when not.
  const full = render(true, true);
  return full.length <= DISCORD_LIMIT ? full : render(false, true);
}

// ── Reddit ──────────────────────────────────────────────────────────────────

function buildReddit() {
  const firstChange = groups[0]?.items[0] ? shortItem(groups[0].items[0]).replace(/:.*$/, "") : null;
  const lede = headline || (summary ? clip(summary.replace(/\.$/, ""), 120) : firstChange);
  const title = `PS5Upload ${tag}${channel} released${lede ? ` — ${lede}` : ""}`;
  const body = [`**PS5Upload ${tag}** is out${prerelease ? " as a pre-release" : ""}.${summary ? ` ${summary}` : ""}`, "", "### What's new", ""];
  for (const g of groups) {
    if (g.heading) body.push("", `#### ${stripMd(g.heading)}`, "");
    for (const it of g.items) body.push(`- ${it.text}`);
  }
  body.push(
    "",
    "### Links",
    "",
    `- **Download:** [ps5upload.phantomptr.com](${SITE_URL}) (picks the right file for your system)`,
    `- **Release notes:** [${tag} on GitHub](${releaseUrl})`,
    `- **Docker (web UI):** \`${DOCKER_IMAGE}:${version}\``,
    `- **Bugs:** [open an issue](${ISSUES_URL})`,
    `- **Help and ideas:** [Discord](${DISCORD_INVITE})`,
    "",
    `PS5Upload is free. If it saves you time, [a coffee](${COFFEE_URL}) keeps it going ☕`,
  );
  return { title: title.slice(0, 300), body: body.join("\n") };
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

fs.mkdirSync(outDir, { recursive: true });
for (const f of fs.readdirSync(outDir)) fs.rmSync(path.join(outDir, f), { recursive: true }); // only this version's files
const files = [];
const write = (name, text) => {
  fs.writeFileSync(path.join(outDir, name), `${text}\n`);
  files.push(name);
};
const xPosts = [x.main, ...x.replies];
xPosts.forEach((t, i) => write(`x-${i + 1}-${i === 0 ? "main" : "reply"}.txt`, t));
write("discord.md", discord);
write("reddit-title.txt", reddit.title);
write("reddit-body.md", reddit.body);
// The website shows this release: its hero carries the version and the download, its News
// section these notes. Taken fresh when Playwright is available.
captureSite();
const images = copyImages(path.join(outDir, "images"), ["site-hero-dark", "site-news-dark", "site-hero-light"]);
write(
  "README.md",
  [
    `# PS5Upload ${tag} release posts`,
    "",
    "Copy each file's contents into the place named. Nothing here is committed (.release_posts is git-ignored).",
    "",
    "## X",
    ...xPosts.map((t, i) =>
      `- \`x-${i + 1}-${i === 0 ? "main" : "reply"}.txt\` — ${i === 0 ? `the post${images.length ? " (attach images/site-hero-dark.png)" : ""}` : `reply ${i} (reply to the previous post)`}, ${xLength(t)}/${X_LIMIT} characters`,
    ),
    "",
    "## Discord",
    `- \`discord.md\` — the announcement, ${discord.length}/${DISCORD_LIMIT} characters. Remove the @everyone line to post without a ping.${images.length ? " Attach images/site-news-dark.png." : ""}`,
    "",
    "## Reddit",
    "- `reddit-title.txt` — the Title box",
    "- `reddit-body.md` — the body (switch the editor to Markdown first)",
    "",
    ...(images.length
      ? ["## Images (from the website)", ...images.map((n) => `- \`images/${n}\``), ""]
      : []),
  ].join("\n"),
);
console.error(`\nWrote ${files.length} files to ${path.relative(repoRoot, outDir) || outDir}`);
