#!/usr/bin/env node
/**
 * "What is PS5Upload" posts for people who have never seen it: the features, the platforms,
 * how to start, and where to download. Not release notes (that's release-posts.js).
 *
 * Usage
 *   make promo-post                         # = node scripts/promo-posts.js
 *   node scripts/promo-posts.js --out DIR   # write elsewhere
 *
 * Writes `.release_posts/about/` (git-ignored), one file per thing you paste, shaped for each
 * platform like release-posts.js does:
 *   x-1-main.txt, x-2-reply.txt, …   plain text, 280 characters each (links count as 23)
 *   discord.md                       Discord markdown, under 2000 characters
 *   reddit-title.txt, reddit-body.md the title box and the Markdown body
 *   images/                          app screenshots to attach (site/assets), as PNG
 *   README.md                        what goes where, with lengths
 *
 * The version and download link come from VERSION; everything else is the text below. Keep it
 * in line with README.md's "What it does" when a big feature lands.
 */
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");
const { captureSite, copyImages } = require("./post-images.js");

const REPO = "phantomptr/ps5upload";
const RELEASES_URL = `https://github.com/${REPO}/releases/latest`;
const REPO_URL = `https://github.com/${REPO}`;
const ISSUES_URL = `https://github.com/${REPO}/issues`;
const DISCORD_INVITE = "https://discord.gg/fzK3xddtrM";
const COFFEE_URL = "https://ko-fi.com/B0B81S0WUA"; // client/src/lib/supportLinks.ts
const SITE_URL = "https://ps5upload.phantomptr.com/"; // site/ on GitHub Pages
const AUTHOR_URL = "https://phantomptr.com/";
const DOCKER_IMAGE = "ghcr.io/phantomptr/ps5upload-engine-webui";
const X_LIMIT = 280;
const X_URL_LEN = 23;
const DISCORD_LIMIT = 2000;

const repoRoot = path.resolve(__dirname, "..");
const args = process.argv.slice(2);
const outIdx = args.indexOf("--out");
const outDir = outIdx >= 0 ? args[outIdx + 1] : path.join(repoRoot, ".release_posts", "about");
const version = fs.readFileSync(path.join(repoRoot, "VERSION"), "utf8").trim();

const PITCH =
  "PS5Upload gets your games, apps and homebrew onto a jailbroken PS5, fast, and lets you manage the console from your computer, phone or browser.";

/** The features, in the order a newcomer cares about them. `x` is the one-line form. */
const FEATURES = [
  {
    title: "Fast uploads that don't give up",
    x: "Fast, encrypted uploads that resume after a dropped connection",
    text: "One encrypted connection at the full speed of your network, every file verified, and an upload that carries on after a dropped connection, a restart or rest mode. Games, folders, disk images and .zip/.7z/.rar archives (unpacked on the way); thousands of small files are packed together so they don't crawl.",
  },
  {
    title: "Install any package",
    x: "Install PS4 and PS5 packages from your computer, a NAS or a link",
    text: "PS4 .pkg and PS5 fake packages (base, update, DLC) straight from your computer while they install, from a NAS or SMB share, a USB drive or a download link. If the console refuses one way, the app offers another.",
  },
  {
    title: "Your whole collection",
    x: "See every game you own, with its updates, DLC and duplicates",
    text: "Every game on your drives and network shares, grouped with its updates and DLC, with what each PS5 already has and one click to install the rest. Find duplicates and free up space.",
  },
  {
    title: "Convert games",
    x: "Turn game folders into packages or ShadowMount+ images",
    text: "Build an installable package from a game folder, or write it as a game image (.exfat, .ffpkg, .ffpfs or the smaller .ffpfsc) that ShadowMount+ mounts, with optional LZ4 asset packs to make images smaller. Then upload it in one step.",
  },
  {
    title: "Manage the console",
    x: "Launch games, back up saves, browse files, copy PS5 to PS5",
    text: "Launch, stop and uninstall games; back up and restore saves; browse files, screenshots and clips; copy straight from one PS5 to another. Plus cheats, fan curve, hardware sensors, a payload sender with a catalogue, and rest/reboot/wake over the network.",
  },
  {
    title: "Runs everywhere",
    x: "macOS, Windows, Linux, Android and Docker, in 21 languages",
    text: "Native apps for macOS, Windows, Linux and Android, and a Docker image that serves the same app as a web page from your NAS. In 21 languages. Free and open source (GPL-3.0).",
  },
];

const START = [
  "Jailbreak the PS5 and keep an ELF loader running (port 9021).",
  "Open PS5Upload, go to Connection and enter the PS5's IP address.",
  "Press Send payload: the app starts its helper on the console and pairs with it by itself.",
];

// ── X ───────────────────────────────────────────────────────────────────────

const xLength = (s) => s.replace(/https?:\/\/\S+/g, "x".repeat(X_URL_LEN)).length;

function buildX() {
  const posts = [
    [
      "Meet PS5Upload 🎮",
      "Get games, apps and homebrew onto a jailbroken PS5, fast, and manage the console from your computer, phone or browser.",
      "Free and open source. macOS, Windows, Linux, Android, Docker.",
      `Download: ${SITE_URL}`,
      "#PS5 #PS5Homebrew",
    ].join("\n\n"),
  ];
  // Feature lines, packed into as few replies as fit.
  let cur = ["What it does:"];
  for (const f of FEATURES) {
    const line = `• ${f.x}`;
    if (xLength([...cur, line].join("\n")) > X_LIMIT) {
      posts.push(cur.join("\n"));
      cur = [];
    }
    cur.push(line);
  }
  posts.push(cur.join("\n"));
  posts.push(
    [
      "Getting started takes a minute:",
      ...START.map((s, i) => `${i + 1}. ${s.replace(" (port 9021)", "")}`),
    ].join("\n"),
  );
  posts.push(
    [`Website: ${SITE_URL}`, `Source: ${REPO_URL}`, `Help & ideas: ${DISCORD_INVITE}`, `☕ ${COFFEE_URL}`].join("\n"),
  );
  return posts;
}

// ── Discord ─────────────────────────────────────────────────────────────────

const firstSentence = (t) => t.split(/(?<=\.)\s/)[0];

function buildDiscord() {
  // Full text if it fits Discord's limit, else each feature's first sentence, else the one-liners.
  const sizes = [(f) => f.text, (f) => firstSentence(f.text), (f) => f.x];
  const render = (body) =>
    [
      `## 🎮 PS5Upload`,
      PITCH,
      "",
      ...FEATURES.map((f) => `- **${f.title}.** ${body(f)}`),
      "",
      "**Getting started**",
      ...START.map((s, i) => `${i + 1}. ${s}`),
      "",
      `**[Download v${version}](<${SITE_URL}>)** · [Source](<${REPO_URL}>) · [Report a bug](<${ISSUES_URL}>) · ☕ [Buy me a coffee](<${COFFEE_URL}>)`,
      `-# Made by [PhantomPtr](<${AUTHOR_URL}>). Free and open source (GPL-3.0). Not affiliated with Sony.`,
    ].join("\n");
  for (const size of sizes) {
    const text = render(size);
    if (text.length <= DISCORD_LIMIT) return text;
  }
  return render(sizes[sizes.length - 1]);
}

// ── Reddit ──────────────────────────────────────────────────────────────────

function buildReddit() {
  const title = "PS5Upload: everything you need for your jailbroken PS5, in one app";
  const body = [
    `${PITCH} I've been building it for a while and it's at **v${version}** now, so here's what it does for anyone who hasn't tried it.`,
    "",
    "### What it does",
    "",
    ...FEATURES.map((f) => `- **${f.title}.** ${f.text}`),
    "",
    "### What it doesn't do",
    "",
    "- It doesn't jailbreak the console: you need a working jailbreak with an ELF loader first.",
    "- It doesn't sign packages with fake keys; it installs packages the console already trusts.",
    "",
    "### Getting started",
    "",
    ...START.map((s, i) => `${i + 1}. ${s}`),
    "",
    "Supported firmware: 1.00 to 14.10 (tested on 5.10, 9.60 and 13.60; 14.x untested). In practice the limit is your jailbreak's ELF loader.",
    "",
    "### Links",
    "",
    `- **Website and downloads:** [ps5upload.phantomptr.com](${SITE_URL})`,
    `- **Docker web UI:** \`${DOCKER_IMAGE}:latest\``,
    `- **Source:** [GitHub](${REPO_URL})`,
    `- **Bugs:** [open an issue](${ISSUES_URL})`,
    `- **Help and ideas:** [Discord](${DISCORD_INVITE})`,
    "",
    `It's free. If it saves you time, [a coffee](${COFFEE_URL}) keeps it going ☕`,
    "",
    `*Made by [PhantomPtr](${AUTHOR_URL}). Not affiliated with Sony. Use it on consoles you own.*`,
  ].join("\n");
  return { title, body };
}

// ── Output ──────────────────────────────────────────────────────────────────

fs.mkdirSync(outDir, { recursive: true });
for (const f of fs.readdirSync(outDir)) fs.rmSync(path.join(outDir, f), { recursive: true });
const written = [];
const write = (name, text) => {
  fs.writeFileSync(path.join(outDir, name), `${text}\n`);
  written.push(name);
};

const x = buildX();
const tooLong = x.map((t, i) => [i + 1, xLength(t)]).filter(([, n]) => n > X_LIMIT);
if (tooLong.length) {
  console.error(`X post(s) over ${X_LIMIT} characters: ${tooLong.map(([i, n]) => `#${i} (${n})`).join(", ")}. Shorten the text in promo-posts.js.`);
  process.exit(1);
}
x.forEach((t, i) => write(`x-${i + 1}-${i === 0 ? "main" : "reply"}.txt`, t));
const discord = buildDiscord();
write("discord.md", discord);
const reddit = buildReddit();
write("reddit-title.txt", reddit.title);
write("reddit-body.md", reddit.body);

// Screenshots to attach: the website (hero, features, downloads), taken fresh when
// Playwright is available, and the app itself (site/assets, as PNG since not every uploader
// takes WebP).
captureSite();
const shots = copyImages(path.join(outDir, "images"), ["site-hero-dark", "site-features-dark", "site-download-dark", "site-hero-light"]);
const imgDir = path.join(outDir, "images");
fs.mkdirSync(imgDir, { recursive: true });
for (const name of ["home-dark", "home-light", "upload-dark", "convert-dark"]) {
  const src = path.join(repoRoot, "site", "assets", `${name}.webp`);
  if (!fs.existsSync(src)) continue;
  const dst = path.join(imgDir, `app-${name}.png`);
  try {
    execFileSync("sips", ["-s", "format", "png", src, "--out", dst], { stdio: "ignore" }); // macOS
    shots.push(path.basename(dst));
  } catch {
    fs.copyFileSync(src, path.join(imgDir, `app-${name}.webp`)); // elsewhere: attach the WebP
    shots.push(`app-${name}.webp`);
  }
}

write(
  "README.md",
  [
    `# PS5Upload introduction posts (v${version})`,
    "",
    "For people who haven't seen PS5Upload yet. Copy each file into the place named; attach a screenshot from images/ where it says so. Nothing here is committed.",
    "",
    "## X",
    ...x.map((t, i) =>
      `- \`x-${i + 1}-${i === 0 ? "main" : "reply"}.txt\` — ${i === 0 ? "the post (attach images/site-hero-dark.png)" : i === 1 ? "reply 1 (attach images/site-features-dark.png)" : `reply ${i}`}, ${xLength(t)}/${X_LIMIT} characters`,
    ),
    "",
    "## Discord",
    `- \`discord.md\` — ${discord.length}/${DISCORD_LIMIT} characters. Attach images/site-hero-dark.png and images/app-home-dark.png.`,
    "",
    "## Reddit",
    "- `reddit-title.txt` — the Title box",
    "- `reddit-body.md` — the body (Markdown editor). Post it as an image post with images/site-hero-dark.png, or add the images in the body.",
    "",
    "## Images",
    ...shots.map((s) => `- \`images/${s}\``),
    "",
  ].join("\n"),
);
console.error(`Wrote ${written.length} files and ${shots.length} images to ${path.relative(repoRoot, outDir) || outDir}`);
