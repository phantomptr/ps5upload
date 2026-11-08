// The FAQ page (faq/index.html, made by scripts/site-faq.mjs from FAQ.md): the language and
// light/dark switch as on the home page, a search over the answers, and opening the question a
// link points at (#<id>, or ?item=<id> / ?topic=<id> as the app's web UI links them).
// Everything is in the page; nothing is fetched but this site's own translations.
"use strict";

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

/** The site's languages (the same list as app.js). */
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

function t(key, en, vars = {}) {
  let s = words[key] ?? en;
  for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, v);
  return s;
}

async function loadWords() {
  if (lang === "en") return;
  try {
    const res = await fetch(`../i18n/${lang}.json`);
    if (res.ok) words = await res.json();
  } catch {
    /* stays English */
  }
}

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
  const label = t("faq.link", "Link to this answer");
  document.querySelectorAll(".faq-link").forEach((a) => {
    a.setAttribute("aria-label", label);
    a.title = label;
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

const systemDark = () => window.matchMedia("(prefers-color-scheme: dark)").matches;
document.getElementById("theme").addEventListener("click", () => {
  const dark = root.dataset.theme ? root.dataset.theme === "dark" : systemDark();
  const next = dark ? "light" : "dark";
  store.set("ps5upload.site.theme", next);
  root.dataset.theme = next;
});

// ── Opening the linked answer ───────────────────────────────────────────────

const body = document.getElementById("faq-body");
const status = document.getElementById("faq-status");

/** Opens the question (or shows the topic) with this id and brings it into view. */
function reveal(id, scroll = true) {
  if (!id) return false;
  const el = document.getElementById(id);
  if (!el || !body.contains(el)) return false;
  if (el.tagName === "DETAILS") el.open = true;
  if (scroll) el.scrollIntoView({ block: "start" });
  return true;
}

function fromAddress() {
  const params = new URLSearchParams(location.search);
  const asked = params.get("item") || params.get("topic");
  if (asked && reveal(asked)) {
    params.delete("item");
    params.delete("topic");
    const rest = params.toString();
    history.replaceState(null, "", `${location.pathname}${rest ? `?${rest}` : ""}#${asked}`);
    return;
  }
  if (location.hash) reveal(decodeURIComponent(location.hash.slice(1)));
}
window.addEventListener("hashchange", () => reveal(decodeURIComponent(location.hash.slice(1))));

// The # beside a question: its link, also copied where the browser allows.
body.addEventListener("click", async (e) => {
  const a = e.target.closest(".faq-link");
  if (!a) return;
  e.preventDefault();
  e.stopPropagation();
  const id = a.getAttribute("href").slice(1);
  history.replaceState(null, "", `#${id}`);
  reveal(id, false);
  try {
    await navigator.clipboard.writeText(location.href);
    status.textContent = t("faq.copied", "Link copied");
  } catch {
    status.textContent = "";
  }
});

// ── Search ─────────────────────────────────────────────────────────────────

const units = [...body.querySelectorAll(".faq-unit")].map((el) => ({ el, text: el.textContent.toLowerCase() }));
const topics = [...body.querySelectorAll(".faq-topic")];
const groups = [...body.querySelectorAll(".faq-group")];
const none = document.getElementById("faq-none");
const input = document.getElementById("faq-search");
let wasOpen = null;

function search(query) {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) {
    for (const u of units) u.el.hidden = false;
    for (const s of [...topics, ...groups]) s.hidden = false;
    // Back to how the questions were before searching.
    if (wasOpen) for (const u of units) if (u.el.tagName === "DETAILS") u.el.open = wasOpen.has(u.el);
    wasOpen = null;
    none.hidden = true;
    status.textContent = "";
    return;
  }
  if (!wasOpen) wasOpen = new Set(units.filter((u) => u.el.open).map((u) => u.el));
  let found = 0;
  for (const u of units) {
    const hit = words.every((w) => u.text.includes(w));
    u.el.hidden = !hit;
    if (hit) found++;
  }
  for (const s of topics) s.hidden = !s.querySelector(".faq-unit:not([hidden])");
  for (const g of groups) g.hidden = !g.querySelector(".faq-topic:not([hidden])");
  // A handful of answers: open them, the search is for reading them.
  for (const u of units) if (u.el.tagName === "DETAILS" && !u.el.hidden) u.el.open = found <= 3;
  none.hidden = found > 0;
  none.textContent = t("faq.none", "No answer has all of these words. Try fewer words, or ask on Discord.");
  status.textContent = found ? t("faq.found", "Matching answers: {n}", { n: found }) : "";
}

let timer = 0;
input.addEventListener("input", () => {
  clearTimeout(timer);
  timer = setTimeout(() => search(input.value), 120);
});
input.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    input.value = "";
    search("");
  }
});

// On a phone the topic list starts closed, so the answers are what's in view.
if (window.matchMedia("(max-width: 900px)").matches) document.getElementById("faq-toc").open = false;

(async () => {
  await loadWords();
  translatePage();
  root.classList.remove("i18n-wait");
  fromAddress();
})();
