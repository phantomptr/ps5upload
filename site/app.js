// PS5Upload website: live releases, the right download for this visitor, the news list, the
// language and the light/dark switch. No build step and no dependencies.
//
// The words come in the visitor's language: English is in index.html, the others in
// i18n/<code>.json (scripts/site-i18n.mjs checks them against the page). The language itself
// is picked in index.html's <head>, before anything is drawn, so the page never flashes the
// wrong way round; each language has its own screenshots, taken from the app in that language.
//
// Releases come from GitHub's API when the page loads, so a new release shows up without a
// redeploy. If that fails (offline, or GitHub's 60-requests-an-hour limit for anonymous
// visitors), releases.json is used: a snapshot the Pages workflow writes on every deploy and
// every published release (scripts/site-releases.mjs). The download buttons offer the newest
// stable release and name a newer pre-release beside it (platform.js picks).
"use strict";

const REPO = "phantomptr/ps5upload";
const API = `https://api.github.com/repos/${REPO}/releases?per_page=12`;
const NEWS_COUNT = 4;

// ── Language ───────────────────────────────────────────────────────────────

const root = document.documentElement;
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

/** The app's languages, in the app's order (client/src/state/lang.ts). */
const LANGS = [
  ["en", "English"],
  ["zh-CN", "简体中文"],
  ["zh-TW", "繁體中文"],
  ["hi", "हिन्दी"],
  ["es", "Español"],
  ["ar", "العربية"],
  ["bn", "বাংলা"],
  ["pl", "Polski"],
  ["pt-BR", "Português (Brasil)"],
  ["ru", "Русский"],
  ["ja", "日本語"],
  ["de", "Deutsch"],
  ["fr", "Français"],
  ["ko", "한국어"],
  ["tr", "Türkçe"],
  ["vi", "Tiếng Việt"],
  ["id", "Bahasa Indonesia"],
  ["it", "Italiano"],
  ["th", "ไทย"],
  ["hu", "Magyar"],
  ["fa", "فارسی"],
];
const lang = LANGS.some(([c]) => c === root.lang) ? root.lang : "en";
let words = {};

/** A string in the visitor's language, `en` when it has no translation; {name} takes vars.name. */
function t(key, en, vars = {}) {
  let s = words[key] ?? en;
  for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, v);
  return s;
}

async function loadWords() {
  if (lang === "en") return;
  try {
    const res = await fetch(`i18n/${lang}.json`);
    if (res.ok) words = await res.json();
  } catch {
    /* stays English */
  }
}

/** Puts the translations into the page: data-i18n is an element's content (which may hold
 *  <b>, <i>, <a> and <span>), data-i18n-attr is "attribute:key" pairs split by ";". */
function translatePage() {
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const s = words[el.dataset.i18n];
    if (s !== undefined) el.innerHTML = s;
  });
  document.querySelectorAll("[data-i18n-attr]").forEach((el) => {
    for (const pair of el.dataset.i18nAttr.split(";")) {
      const [attr, key] = pair.split(":");
      if (words[key] !== undefined) el.setAttribute(attr, words[key]);
    }
  });
}

const select = document.getElementById("lang");
select.innerHTML = LANGS.map(([c, label]) => `<option value="${c}" lang="${c}">${label}</option>`).join("");
select.value = lang;
select.addEventListener("change", () => {
  store.set("ps5upload.site.lang", select.value);
  const url = new URL(location.href);
  url.searchParams.set("lang", select.value);
  location.href = url.href;
});

// ── Theme ──────────────────────────────────────────────────────────────────

const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;

function applyTheme(theme) {
  if (theme) root.dataset.theme = theme;
  else delete root.dataset.theme;
  const dark = theme ? theme === "dark" : systemDark();
  // The screenshots follow the page, not only the system, and are in the page's language.
  const dir = lang === "en" ? "assets/shots/" : `assets/shots/${lang}/`;
  document.querySelectorAll("[data-theme-src]").forEach((s) => {
    s.srcset = `${dir}${s.dataset.themeSrc}-dark.webp`;
    s.media = theme ? "not all" : "(prefers-color-scheme: dark)";
  });
  document.querySelectorAll("img[data-theme-img]").forEach((img) => {
    img.src = `${dir}${img.dataset.themeImg}-${dark ? "dark" : "light"}.webp`;
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
    btn.textContent = t("js.copied", "Copied");
  } catch {
    btn.textContent = t("js.copy_failed", "Select and copy");
  }
  setTimeout(() => (btn.textContent = t("download.copy", "Copy")), 1600);
});

// ── Releases ───────────────────────────────────────────────────────────────

/** What a release file is, from its name (scripts/gen-updater-manifest.py names them). */
const KINDS = [
  { re: /-mac-arm64\.dmg$/, os: "mac", label: () => "Apple Silicon", note: ".dmg" },
  { re: /-mac-x64\.dmg$/, os: "mac", label: () => "Intel", note: ".dmg" },
  { re: /-win-x64-setup\.exe$/, os: "windows", label: () => t("dl.win_x64_setup", "x64 installer"), note: ".exe" },
  { re: /-win-x64\.zip$/, os: "windows", label: () => t("dl.win_x64_zip", "x64 portable"), note: ".zip" },
  { re: /-win-arm64-setup\.exe$/, os: "windows", label: () => t("dl.win_arm64_setup", "ARM64 installer"), note: ".exe" },
  { re: /-win-arm64\.zip$/, os: "windows", label: () => t("dl.win_arm64_zip", "ARM64 portable"), note: ".zip" },
  { re: /-linux-x64\.deb$/, os: "linux", label: () => t("dl.linux_x64_deb", "x64 Debian, Ubuntu"), note: ".deb" },
  { re: /-linux-x64\.rpm$/, os: "linux", label: () => t("dl.linux_x64_rpm", "x64 Fedora, openSUSE"), note: ".rpm" },
  { re: /-linux-x64\.AppImage$/, os: "linux", label: () => "x64 AppImage", note: ".AppImage" },
  { re: /-linux-x64\.zip$/, os: "linux", label: () => t("dl.linux_x64_zip", "x64 any distro"), note: ".zip" },
  { re: /-linux-arm64\.deb$/, os: "linux", label: () => t("dl.linux_arm64_deb", "ARM64 Debian, Ubuntu"), note: ".deb" },
  { re: /-linux-arm64\.rpm$/, os: "linux", label: () => t("dl.linux_arm64_rpm", "ARM64 Fedora, openSUSE"), note: ".rpm" },
  { re: /-linux-arm64\.AppImage$/, os: "linux", label: () => "ARM64 AppImage", note: ".AppImage" },
  { re: /-linux-arm64\.zip$/, os: "linux", label: () => t("dl.linux_arm64_zip", "ARM64 any distro"), note: ".zip" },
  { re: /-android\.apk$/, os: "android", label: () => t("dl.android", "Phones and tablets"), note: ".apk" },
  { re: /-android-armv7\.apk$/, os: "android", label: () => t("dl.android_armv7", "Older 32-bit phones"), note: ".apk" },
  { re: /-ios-unsigned\.ipa$/, os: "ios", label: () => t("dl.ios", "iPhone, sideload"), note: ".ipa" },
  { re: /^ps5upload-[\d.]+\.elf$/, os: "extra", label: () => t("dl.payload", "PS5 helper (payload)"), note: ".elf" },
  { re: /^ps5upload-engine-/, os: "extra", label: null, note: "" },
];
const OS = {
  mac: "macOS",
  windows: "Windows",
  linux: "Linux",
  android: "Android",
  ios: "iPhone",
};

/** Chromium's exact CPU, which its user agent no longer tells. Never holds the page up. */
async function browserHints() {
  try {
    const asked = navigator.userAgentData?.getHighEntropyValues(["architecture"]);
    if (!asked) return null;
    return await Promise.race([asked, new Promise((done) => setTimeout(() => done(null), 500))]);
  } catch {
    return null;
  }
}

/** A file size in the page's language: 1.2 GB, 1,2 GB, 1,2 Go. */
const mb = (n) => {
  const big = n >= 1e9;
  const value = big ? n / 1e9 : Math.max(1, Math.round(n / 1e6));
  const digits = big ? 1 : 0;
  try {
    return new Intl.NumberFormat(lang, {
      style: "unit",
      unit: big ? "gigabyte" : "megabyte",
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    }).format(value);
  } catch {
    // Browsers from before Intl's unit style.
    return `${value.toFixed(digits)} ${big ? "GB" : "MB"}`;
  }
};
const date = (iso) =>
  iso ? new Date(iso).toLocaleDateString(lang, { year: "numeric", month: "short", day: "numeric" }) : "";

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
  const headers = { Accept: "application/vnd.github+json" };
  try {
    const res = await fetch(API, { signal: ctl.signal, headers });
    if (!res.ok) throw new Error(String(res.status));
    const list = normalize(await res.json());
    if (!list.length) throw new Error("empty");
    // A run of pre-releases can push the stable one out of the newest twelve.
    if (list.every((r) => r.prerelease)) {
      const more = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { signal: ctl.signal, headers })
        .then((r) => (r.ok ? r.json() : null))
        .catch(() => null);
      if (more) list.push(...normalize([more]));
    }
    return list;
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
    groups[k.os].push({ ...a, order: i, label: k.label ? k.label() : a.name.replace(/^ps5upload-engine-[\d.]+-/, t("js.engine", "Engine") + " · "), note: k.note });
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
        <div class="dl-head"><h3>${OS[os]}</h3>${os === mine ? `<span class="tag">${t("js.your_system", "Your system")}</span>` : ""}</div>
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

function renderHero(rel, newerPre, groups, platform) {
  const mine = platform.os;
  const pre = rel.prerelease ? ` · ${t("js.prerelease_lower", "pre-release")}` : "";
  document.getElementById("hero-version").textContent = `${rel.tag}${pre} · ${date(rel.published)}`;
  const files = mine ? pickFiles(groups[mine], platform) : [];
  const btn = document.getElementById("hero-download");
  const others = `<a href="#download">${t("js.other_platforms", "Other platforms")}</a>`;
  if (files.length) {
    document.getElementById("hero-download-label").textContent = t("js.download_for", "Download for {os}", { os: OS[mine] });
    if (files.length === 1) {
      const [only] = files;
      btn.href = only.url;
      document.getElementById("hero-fine").innerHTML = `${esc(only.label)} ${esc(only.note)} · ${mb(only.size)} · ${others}`;
    } else {
      // Can't tell which one (a Mac's chip in Safari): both, rather than a guess that won't open.
      btn.href = "#download";
      document.getElementById("hero-fine").innerHTML = `${files
        .map((f) => `<a href="${esc(f.url)}">${esc(f.label)} ${esc(f.note)}</a> · ${mb(f.size)}`)
        .join(" · ")} · ${others}`;
    }
  } else {
    btn.href = "#download";
    document.getElementById("hero-download-label").textContent = t("hero.download", "Download");
  }
  document.getElementById("release-link").href = rel.url;
  const lead = document.getElementById("download-lead");
  const preLine = (r) => t("js.lead_pre", "{tag} is a pre-release: new and being tested.", { tag: r.tag });
  lead.textContent = rel.prerelease
    ? preLine(rel)
    : newerPre
      ? `${t("js.lead_stable", "The last stable release is {tag}.", { tag: rel.tag })} ${preLine(newerPre)}`
      : t("js.lead_released", "{tag}, released {date}.", { tag: rel.tag, date: date(rel.published) });
  if (newerPre) {
    lead.insertAdjacentHTML("beforeend", ` <a href="${esc(newerPre.url)}">${esc(t("js.get", "Get {tag}", { tag: newerPre.tag }))}</a>`);
  }
  // The same example values as engine/compose.yaml, so it pastes as it is and only the addresses
  // and the folder need changing.
  document.getElementById("docker-cmd").textContent =
    `docker run -d --name ps5upload --network host -v ps5upload-data:/data -v /srv/ps5/pkgs:/pkgs:ro -e PS5_ADDR=192.168.1.50 -e PS5UPLOAD_ALLOW_IP=192.168.1.0/24 -e PS5UPLOAD_BROWSE_ROOTS=/pkgs ghcr.io/${REPO.split("/")[0]}/ps5upload-engine-webui:${rel.tag.replace(/^v/, "")}`;
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
          ${r.prerelease ? `<span class="tag pre">${t("js.prerelease", "Pre-release")}</span>` : `<span class="tag">${t("js.release", "Release")}</span>`}
          <time datetime="${esc(r.published || "")}">${date(r.published)}</time>
        </div>
        <div class="post-body">${markdown(excerpt(r.body).md)}</div>
        <a class="post-more" href="${esc(r.url)}">${excerpt(r.body).more ? t("js.read_rest", "Read the rest, and downloads →") : t("js.full_notes", "Downloads and full notes →")}</a>
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

// ── Thanks & Support ──────────────────────────────────────────────────────

// Public receiving addresses (the owner's hardware wallet), the same as the app's
// client/src/data/donate.ts. QR codes are assets/donate/<id>.svg.
const CRYPTO = [
  { id: "btc", coin: "BTC", name: "Bitcoin", network: "Bitcoin", color: "#f7931a", address: "bc1q36gxkvd3wfxy9azm8hft3rru9m6m9hjvn7u20n" },
  { id: "eth", coin: "ETH", name: "Ethereum", network: "Ethereum", color: "#627eea", address: "0x2e787654db543B69FB011E16995C274dFAfc2286" },
  { id: "sol", coin: "SOL", name: "Solana", network: "Solana", color: "#9945ff", address: "G1q5XCCn3mePYpcx7uY17CwDsUaAZTcty56huUDvVqjG" },
  { id: "xrp", coin: "XRP", name: "XRP", network: "XRP Ledger", color: "#2a9df4", address: "rEVq3si3sm9NuWNuZiUZFKsUtwC3w9SrjR" },
];

function showCoin(id) {
  const c = CRYPTO.find((x) => x.id === id) ?? CRYPTO[0];
  document.querySelectorAll("#coin-tabs button").forEach((b) => {
    const on = b.dataset.coin === c.id;
    b.setAttribute("aria-selected", String(on));
    b.tabIndex = on ? 0 : -1;
    b.style.background = on ? c.color : "";
    b.style.color = on ? "#fff" : "";
  });
  document.getElementById("coin-panel").setAttribute("aria-labelledby", `coin-tab-${c.id}`);
  const qr = document.getElementById("coin-qr");
  qr.src = `assets/donate/${c.id}.svg`;
  qr.alt = `${c.name} ${t("thanks.qr", "QR code")}`;
  qr.style.borderColor = c.color;
  const name = document.getElementById("coin-name");
  name.textContent = c.name;
  name.style.color = c.color;
  document.getElementById("coin-network").textContent = t("thanks.network", "Network: {network}", { network: c.network });
  document.getElementById("coin-address").textContent = c.address;
  document.getElementById("coin-warn").textContent = t(
    "thanks.network_only",
    "Send only {coin} on the {network} network. Anything else can't be recovered.",
    { coin: c.coin, network: c.network },
  );
  document.getElementById("coin-note").hidden = c.id !== "xrp";
}

function setupThanks() {
  const tabs = document.getElementById("coin-tabs");
  tabs.innerHTML = CRYPTO.map(
    (c) => `<button type="button" role="tab" id="coin-tab-${c.id}" aria-controls="coin-panel" data-coin="${c.id}">${c.coin}</button>`,
  ).join("");
  tabs.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-coin]");
    if (b) showCoin(b.dataset.coin);
  });
  // A tab list is one stop for Tab; the arrows move inside it.
  tabs.addEventListener("keydown", (e) => {
    const all = [...tabs.querySelectorAll("button[data-coin]")];
    const at = all.indexOf(document.activeElement);
    if (at < 0) return;
    const rtl = root.dir === "rtl";
    const steps = { ArrowRight: rtl ? -1 : 1, ArrowLeft: rtl ? 1 : -1 };
    let next = null;
    if (e.key in steps) next = all[(at + steps[e.key] + all.length) % all.length];
    else if (e.key === "Home") next = all[0];
    else if (e.key === "End") next = all[all.length - 1];
    if (!next) return;
    e.preventDefault();
    showCoin(next.dataset.coin);
    next.focus();
  });
  showCoin("btc");
  const copy = document.getElementById("coin-copy");
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(document.getElementById("coin-address").textContent);
      copy.textContent = t("thanks.copied", "Copied");
      setTimeout(() => (copy.textContent = t("thanks.copy", "Copy")), 1500);
    } catch {
      /* the address is on screen to copy by hand */
    }
  });
}

/** The wall, from supporters.json (names only, most first: scripts/supporters.mjs). */
async function loadSupporters() {
  try {
    const res = await fetch("supporters.json", { cache: "no-cache" });
    if (!res.ok) return;
    const list = await res.json();
    const names = Array.isArray(list.names) ? list.names : [];
    const named = names.length;
    document.getElementById("thanks-wall").innerHTML = `<ul>${names.map((n) => `<li>${esc(n)}</li>`).join("")}</ul>`;
    const anon = Number(list.anonymous) || 0;
    document.getElementById("thanks-counts").innerHTML =
      `<span>♥ ${esc(t("thanks.count_named", "{n} supporters", { n: named }))}</span>` +
      (anon ? `<span>✦ ${esc(t("thanks.count_anonymous", "+{n} anonymous", { n: anon }))}</span>` : "");
    document.getElementById("thanks-anon").textContent = anon
      ? t("thanks.anonymous", "…and {n} more who chose to stay anonymous. Thank you too.", { n: anon })
      : "";
  } catch {
    /* the support ways still show */
  }
}

(async () => {
  await loadWords();
  translatePage();
  root.classList.remove("i18n-wait");
  setupThanks();
  void loadSupporters();
  try {
    const list = await loadReleases();
    if (!list.length) throw new Error("no releases");
    const { main, pre } = chooseReleases(list);
    const platform = detectPlatform(navigator, await browserHints());
    const groups = renderDownloads(main, platform.os);
    renderHero(main, pre, groups, platform);
    renderNews(list);
  } catch {
    const msg = `<p class="muted">${t("js.load_failed", "Couldn't load the releases here.")} <a href="https://github.com/${REPO}/releases">${t("js.see_github", "See them on GitHub")}</a>.</p>`;
    document.getElementById("downloads").innerHTML = msg;
    document.getElementById("news-list").innerHTML = msg;
  }
})();
