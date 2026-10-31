// PS5Upload website: live releases, the right download for this visitor, the news list, and
// the light/dark switch. No build step and no dependencies.
//
// Releases come from GitHub's API when the page loads, so a new release shows up without a
// redeploy. If that fails (offline, or GitHub's 60-requests-an-hour limit for anonymous
// visitors), releases.json is used: a snapshot the Pages workflow writes on every deploy and
// every published release (scripts/site-releases.mjs).
"use strict";

const REPO = "phantomptr/ps5upload";
const API = `https://api.github.com/repos/${REPO}/releases?per_page=12`;
const NEWS_COUNT = 4;

// ── Theme ──────────────────────────────────────────────────────────────────

const root = document.documentElement;
const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;
const store = {
  get: (k) => {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  },
  set: (k, v) => {
    try {
      localStorage.setItem(k, v);
    } catch {
      /* private window: the choice lasts this visit */
    }
  },
};

function applyTheme(theme) {
  if (theme) root.dataset.theme = theme;
  else delete root.dataset.theme;
  const dark = theme ? theme === "dark" : systemDark();
  // The screenshots follow the page, not only the system.
  document.querySelectorAll("[data-theme-src]").forEach((s) => {
    s.media = theme ? "not all" : "(prefers-color-scheme: dark)";
  });
  document.querySelectorAll("img[data-theme-img]").forEach((img) => {
    img.src = `assets/shots/${img.dataset.themeImg}-${dark ? "dark" : "light"}.webp`;
  });
}
applyTheme(store.get("ps5upload.site.theme"));
document.getElementById("theme").addEventListener("click", () => {
  const dark = root.dataset.theme ? root.dataset.theme === "dark" : systemDark();
  const next = dark ? "light" : "dark";
  store.set("ps5upload.site.theme", next);
  applyTheme(next);
});

// ── Copy buttons ───────────────────────────────────────────────────────────

document.addEventListener("click", async (e) => {
  const btn = e.target.closest("[data-copy]");
  if (!btn) return;
  const text = document.getElementById(btn.dataset.copy)?.textContent ?? "";
  try {
    await navigator.clipboard.writeText(text);
    btn.textContent = "Copied";
  } catch {
    btn.textContent = "Select and copy";
  }
  setTimeout(() => (btn.textContent = "Copy"), 1600);
});

// ── Releases ───────────────────────────────────────────────────────────────

/** What a release file is, from its name (scripts/gen-updater-manifest.py names them). */
const KINDS = [
  { re: /-mac-arm64\.dmg$/, os: "mac", label: "Apple Silicon", note: ".dmg" },
  { re: /-mac-x64\.dmg$/, os: "mac", label: "Intel", note: ".dmg" },
  { re: /-win-x64-setup\.exe$/, os: "windows", label: "x64 installer", note: ".exe" },
  { re: /-win-x64\.zip$/, os: "windows", label: "x64 portable", note: ".zip" },
  { re: /-win-arm64-setup\.exe$/, os: "windows", label: "ARM64 installer", note: ".exe" },
  { re: /-win-arm64\.zip$/, os: "windows", label: "ARM64 portable", note: ".zip" },
  { re: /-linux-x64\.deb$/, os: "linux", label: "x64 Debian, Ubuntu", note: ".deb" },
  { re: /-linux-x64\.rpm$/, os: "linux", label: "x64 Fedora, openSUSE", note: ".rpm" },
  { re: /-linux-x64\.AppImage$/, os: "linux", label: "x64 AppImage", note: ".AppImage" },
  { re: /-linux-x64\.zip$/, os: "linux", label: "x64 any distro", note: ".zip" },
  { re: /-linux-arm64\.deb$/, os: "linux", label: "ARM64 Debian, Ubuntu", note: ".deb" },
  { re: /-linux-arm64\.rpm$/, os: "linux", label: "ARM64 Fedora, openSUSE", note: ".rpm" },
  { re: /-linux-arm64\.AppImage$/, os: "linux", label: "ARM64 AppImage", note: ".AppImage" },
  { re: /-linux-arm64\.zip$/, os: "linux", label: "ARM64 any distro", note: ".zip" },
  { re: /-android\.apk$/, os: "android", label: "Phones and tablets", note: ".apk" },
  { re: /-android-armv7\.apk$/, os: "android", label: "Older 32-bit phones", note: ".apk" },
  { re: /-ios-unsigned\.ipa$/, os: "ios", label: "iPhone, sideload", note: ".ipa" },
  { re: /^ps5upload-[\d.]+\.elf$/, os: "extra", label: "PS5 helper (payload)", note: ".elf" },
  { re: /^ps5upload-engine-/, os: "extra", label: null, note: "" },
];
const OS = {
  mac: "macOS",
  windows: "Windows",
  linux: "Linux",
  android: "Android",
  ios: "iPhone",
};

function detectOs() {
  const ua = navigator.userAgent;
  if (/android/i.test(ua)) return "android";
  if (/iphone|ipad|ipod/i.test(ua)) return "ios";
  if (/windows/i.test(ua)) return "windows";
  if (/mac os x|macintosh/i.test(ua) && !/iphone|ipad/i.test(ua)) return "mac";
  if (/linux|x11/i.test(ua)) return "linux";
  return null;
}

const mb = (n) => (n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(n / 1e6))} MB`);
const date = (iso) =>
  iso ? new Date(iso).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }) : "";

function normalize(list) {
  return (list || [])
    .filter((r) => !r.draft)
    .map((r) => ({
      tag: r.tag_name,
      prerelease: !!r.prerelease,
      published: r.published_at,
      body: r.body || "",
      url: r.html_url,
      assets: (r.assets || []).map((a) => ({ name: a.name, url: a.browser_download_url, size: a.size })),
    }));
}

async function loadReleases() {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 6000);
  try {
    const res = await fetch(API, { signal: ctl.signal, headers: { Accept: "application/vnd.github+json" } });
    if (!res.ok) throw new Error(String(res.status));
    const list = normalize(await res.json());
    if (list.length) return list;
    throw new Error("empty");
  } catch {
    const res = await fetch("releases.json", { cache: "no-cache" });
    return normalize(await res.json());
  } finally {
    clearTimeout(timer);
  }
}

// ── Markdown, the little the release notes use ─────────────────────────────

const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
function inline(s) {
  return esc(s)
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2">$1</a>');
}
/** The start of a release's notes for the news list: everything up to its first `maxItems`
 *  bullets. `more` says whether anything was left out. */
function excerpt(md, maxItems = 6) {
  const lines = md.split(/\r?\n/);
  let items = 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim().startsWith("- ") && ++items > maxItems) {
      return { md: lines.slice(0, i).join("\n"), more: true };
    }
  }
  return { md, more: false };
}

/** Paragraphs and "- " bullets (with wrapped continuation lines); headings become bold lines. */
function markdown(md) {
  const out = [];
  let list = null;
  let para = null;
  const flush = () => {
    if (para) out.push(`<p>${inline(para)}</p>`);
    para = null;
  };
  for (const raw of md.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line === "---") {
      flush();
      if (list) out.push(`<ul>${list.map((i) => `<li>${inline(i)}</li>`).join("")}</ul>`);
      list = null;
      continue;
    }
    if (line.startsWith("- ")) {
      flush();
      (list ??= []).push(line.slice(2));
    } else if (list && /^\s{2,}/.test(raw)) {
      list[list.length - 1] += ` ${line}`;
    } else if (/^#{1,6}\s/.test(line)) {
      flush();
      out.push(`<p><strong>${inline(line.replace(/^#+\s*/, ""))}</strong></p>`);
    } else {
      para = para ? `${para} ${line}` : line;
    }
  }
  flush();
  if (list) out.push(`<ul>${list.map((i) => `<li>${inline(i)}</li>`).join("")}</ul>`);
  return out.join("");
}

// ── Render ─────────────────────────────────────────────────────────────────

function renderDownloads(rel, mine) {
  const groups = { mac: [], windows: [], linux: [], android: [], ios: [], extra: [] };
  for (const a of rel.assets) {
    const i = KINDS.findIndex((k) => k.re.test(a.name));
    if (i < 0) continue;
    const k = KINDS[i];
    groups[k.os].push({ ...a, order: i, label: k.label ?? a.name.replace(/^ps5upload-engine-[\d.]+-/, "Engine · "), note: k.note });
  }
  // KINDS order (x64 before ARM64, installer before portable), not GitHub's alphabetical one.
  for (const g of Object.values(groups)) g.sort((a, b) => a.order - b.order);
  const order = ["mac", "windows", "linux", "android", "ios"].sort((a, b) => (a === mine ? -1 : b === mine ? 1 : 0));
  const el = document.getElementById("downloads");
  el.innerHTML = order
    .filter((os) => groups[os].length)
    .map(
      (os) => `
      <div class="card dl${os === mine ? " mine" : ""}">
        <div class="dl-head"><h3>${OS[os]}</h3>${os === mine ? '<span class="tag">Your system</span>' : ""}</div>
        <div class="dl-files">
          ${groups[os]
            .map(
              (f) =>
                `<a class="dl-file" href="${esc(f.url)}"><span>${esc(f.label)} <small>${esc(f.note)}</small></span><small>${mb(f.size)}</small></a>`,
            )
            .join("")}
        </div>
      </div>`,
    )
    .join("");
  document.getElementById("extras").innerHTML = groups.extra
    .map((f) => `<a class="btn small" href="${esc(f.url)}">${esc(f.label)} · ${mb(f.size)}</a>`)
    .join("");
  return groups;
}

function renderHero(rel, stable, groups, mine) {
  const pre = rel.prerelease ? " · pre-release" : "";
  document.getElementById("hero-version").textContent = `${rel.tag}${pre} · ${date(rel.published)}`;
  const first = mine && groups[mine]?.[0];
  const btn = document.getElementById("hero-download");
  if (first) {
    btn.href = first.url;
    document.getElementById("hero-download-label").textContent = `Download for ${OS[mine]}`;
    document.getElementById("hero-fine").innerHTML =
      `${esc(first.label)} ${esc(first.note)} · ${mb(first.size)} · <a href="#download">Other platforms</a>`;
  } else {
    btn.href = "#download";
    document.getElementById("hero-download-label").textContent = "Download";
  }
  document.getElementById("release-link").href = rel.url;
  const lead = document.getElementById("download-lead");
  lead.textContent = rel.prerelease
    ? `${rel.tag} is a pre-release: new and being tested.${stable ? ` The last stable release is ${stable.tag}.` : ""}`
    : `${rel.tag}, released ${date(rel.published)}.`;
  if (rel.prerelease && stable) {
    lead.insertAdjacentHTML("beforeend", ` <a href="${esc(stable.url)}">Get ${esc(stable.tag)}</a>`);
  }
  document.getElementById("docker-cmd").textContent =
    `docker run -d --name ps5upload --network host -v ps5upload-data:/data -e PS5_ADDR=<your PS5's IP> -e PS5UPLOAD_ALLOW_IP=<your LAN, e.g. 192.168.1.0/24> ghcr.io/${REPO.split("/")[0]}/ps5upload-engine-webui:${rel.tag.replace(/^v/, "")}`;
}

function renderNews(list) {
  const el = document.getElementById("news-list");
  const shown = list.slice(0, NEWS_COUNT);
  el.innerHTML =
    shown
      .map(
        (r) => `
      <article class="card">
        <div class="post-head">
          <h3>${esc(r.tag)}</h3>
          ${r.prerelease ? '<span class="tag pre">Pre-release</span>' : '<span class="tag">Release</span>'}
          <time datetime="${esc(r.published || "")}">${date(r.published)}</time>
        </div>
        <div class="post-body">${markdown(excerpt(r.body).md)}</div>
        <a class="post-more" href="${esc(r.url)}">${excerpt(r.body).more ? "Read the rest, and downloads →" : "Downloads and full notes →"}</a>
      </article>`,
      )
      .join("") +
    (list.length > NEWS_COUNT
      ? `<div class="older">${list
          .slice(NEWS_COUNT)
          .map((r) => `<a class="btn small" href="${esc(r.url)}">${esc(r.tag)}</a>`)
          .join("")}</div>`
      : "");
}

(async () => {
  try {
    const list = await loadReleases();
    if (!list.length) throw new Error("no releases");
    const latest = list[0];
    const stable = list.find((r) => !r.prerelease) ?? null;
    const mine = detectOs();
    const groups = renderDownloads(latest, mine);
    renderHero(latest, stable && stable !== latest ? stable : null, groups, mine);
    renderNews(list);
  } catch {
    const msg = `<p class="muted">Couldn't load the releases here. <a href="https://github.com/${REPO}/releases">See them on GitHub</a>.</p>`;
    document.getElementById("downloads").innerHTML = msg;
    document.getElementById("news-list").innerHTML = msg;
  }
})();
