#!/usr/bin/env node
// The website's FAQ page (site/faq/index.html), made from the repo's FAQ.md on every deploy
// (.github/workflows/pages.yml). FAQ.md stays the one source: the app bundles it, GitHub shows it,
// and this turns it into a page with search, a link to every question and questions that open.
//
//   node scripts/site-faq.mjs           write site/faq/index.html (not committed: .gitignore)
//   node scripts/site-faq.mjs --check   only parse and render, to see that it still works
//
// The page around the answers is scripts/site-faq.html: its words are the site's own
// (data-i18n, translated in site/i18n/<code>.json and checked by scripts/site-i18n.mjs), and
// site/faq.js runs it (language, theme, search, opening the linked question). The answers stay
// in English.
//
// Questions and topics get the ids the app's FAQ screen gives them (client/src/lib/faqDoc.ts:
// parseFaq and slugOf, kept the same here; client/src/lib/faqSiteParity.test.ts compares them),
// so `faq/#<id>` here and the web UI's `/faq?item=<id>` name the same answer. The page also takes
// `?item=<id>` and `?topic=<id>`.
//
// The markdown is turned into HTML here, by a small renderer for what FAQ.md uses (headings,
// paragraphs, lists, code, tables, quotes, bold, italic, links). Everything from the markdown is
// escaped: no HTML from FAQ.md ever reaches the page, and only http(s), mailto and in-page links
// are made into links.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const ROOT = new URL("../", import.meta.url);
const REPO_FILES = "https://github.com/phantomptr/ps5upload/blob/main/";

// ── FAQ.md as data: the same as client/src/lib/faqDoc.ts ────────────────────

/** A link-safe id from a heading or question (faqDoc.ts slugOf). */
export function slugOf(text) {
  return (
    text
      .toLowerCase()
      .replace(/[`*_]/g, "")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 80) || "section"
  );
}

const Q_START = /^\*\*Q:\s*/;

/** Groups (`# `), topics (`## `) and their questions (`**Q: …**`), as faqDoc.ts parseFaq. */
export function parseFaq(md) {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const doc = { intro: "", groups: [], topics: [] };
  let group = "";
  const introLines = [];
  let topic = null;
  let item = null;
  let buf = [];
  const seen = new Set();
  const unique = (id) => {
    let out = id;
    for (let n = 2; seen.has(out); n++) out = `${id}-${n}`;
    seen.add(out);
    return out;
  };
  const flush = () => {
    const text = buf
      .join("\n")
      .trim()
      .replace(/(?:^|\n)\s*-{3,}$/, "")
      .trim();
    buf = [];
    if (item) item.answer = text;
    else if (topic) topic.intro = text;
  };
  let fenced = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
    if (!fenced && i > 0 && /^# [^#]/.test(line)) {
      flush();
      item = null;
      topic = null;
      group = line.slice(2).trim();
      if (!doc.groups.includes(group)) doc.groups.push(group);
      continue;
    }
    if (!fenced && line.startsWith("## ")) {
      flush();
      item = null;
      const title = line.slice(3).trim();
      topic = { id: unique(slugOf(title)), title, group, intro: "", items: [] };
      doc.topics.push(topic);
      continue;
    }
    if (!topic) {
      if (i > 0 && group === "") introLines.push(line);
      continue;
    }
    if (!fenced && Q_START.test(line)) {
      flush();
      let q = line.replace(Q_START, "");
      while (!/\*\*\s*$/.test(q) && i + 1 < lines.length && lines[i + 1].trim() !== "") {
        q += ` ${lines[++i].trim()}`;
      }
      const question = q
        .replace(/\*\*\s*$/, "")
        .replace(/`/g, "")
        .trim();
      item = { id: unique(`${topic.id}--${slugOf(question)}`), question, answer: "" };
      topic.items.push(item);
      continue;
    }
    buf.push(line);
  }
  flush();
  doc.intro = introLines.join("\n").trim();
  return doc;
}

// ── Markdown to safe HTML ──────────────────────────────────────────────────

const esc = (s) => s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** GitHub's anchor for a heading (FAQ.md links between its own sections that way). */
const githubSlug = (title) =>
  title
    .toLowerCase()
    .trim()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");

/** Where a link in FAQ.md goes on the site, or null for one that is not followed. */
function linkTarget(url, anchors) {
  if (/^(?:https?:|mailto:)/i.test(url)) return url;
  if (url.startsWith("#")) {
    const id = anchors.get(url.slice(1).toLowerCase());
    return id ? `#${id}` : `${REPO_FILES}FAQ.md${url}`;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(url)) return null;
  return REPO_FILES + url.replace(/^(?:\.?\/)+/, "");
}

/** Bare web addresses in plain text, as GitHub links them. */
function autolink(text) {
  let out = "";
  let last = 0;
  for (const m of text.matchAll(/https?:\/\/[^\s<>"'`]+/g)) {
    const url = m[0].replace(/[.,;:!?)\]]+$/, "");
    out += esc(text.slice(last, m.index)) + `<a href="${esc(url)}">${esc(url)}</a>`;
    last = m.index + url.length;
  }
  return out + esc(text.slice(last));
}

const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|<(https?:\/\/[^\s>]+|mailto:[^\s>]+)>|\[((?:[^\[\]]|\[[^\]]*\])+)\]\(([^)\s]+)(?:\s+"[^"]*")?\)|\*\*(?=\S)([\s\S]+?)\*\*|__(?=\S)([\s\S]+?)__|\*(?=[^\s*])([^*]+?)\*|_(?=[^\s_])([^_]+?)_/g;

/** Inline markdown: code, links, bold and italic; everything else escaped. */
function inline(text, anchors, inLink = false) {
  let out = "";
  let last = 0;
  // Its own copy: the calls nest (bold inside a link), and each needs its own place.
  const re = new RegExp(INLINE.source, "g");
  for (let m; (m = re.exec(text)); ) {
    const at = m.index;
    const token = m[0];
    // `_` inside a word is part of a name (PS5UPLOAD_ALLOW_IP), not emphasis.
    if (m[9] !== undefined && (/[\p{L}\p{N}]$/u.test(text.slice(0, at)) || /^[\p{L}\p{N}]/u.test(text.slice(at + token.length)))) {
      re.lastIndex = at + 1;
      continue;
    }
    const plain = text.slice(last, at);
    out += inLink ? esc(plain) : autolink(plain);
    last = at + token.length;
    if (m[1]) out += `<code>${esc(m[2].replace(/^ (.*) $/, "$1"))}</code>`;
    else if (m[3]) out += inLink ? esc(m[3]) : `<a href="${esc(m[3])}">${esc(m[3].replace(/^mailto:/, ""))}</a>`;
    else if (m[4] !== undefined) {
      const href = linkTarget(m[5], anchors);
      const label = inline(m[4], anchors, true);
      out += href && !inLink ? `<a href="${esc(href)}">${label}</a>` : label;
    } else if (m[6] !== undefined || m[7] !== undefined) out += `<strong>${inline(m[6] ?? m[7], anchors, inLink)}</strong>`;
    else out += `<em>${inline(m[8] ?? m[9], anchors, inLink)}</em>`;
  }
  const rest = text.slice(last);
  return out + (inLink ? esc(rest) : autolink(rest));
}

const indentOf = (line) => line.match(/^ */)[0].length;
const dedent = (line, n) => line.replace(new RegExp(`^ {0,${n}}`), "");
const LIST_ITEM = /^( *)([-*+]|\d+[.)])\s+/;
const FENCE = /^( *)(```+|~~~+)\s*([\w+-]*)/;
const isTableSep = (line) => /^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$/.test(line);
const cells = (row) =>
  row
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split(/(?<!\\)\|/)
    .map((c) => c.trim().replace(/\\\|/g, "|"));

/** Block markdown to HTML. `level` is the heading level a `#` maps to (an answer sits under h3). */
export function markdown(md, anchors = new Map()) {
  const lines = md.replace(/\t/g, "    ").split("\n");
  const out = [];
  let i = 0;
  const startsBlock = (line) =>
    FENCE.test(line) || LIST_ITEM.test(line) || /^ {0,3}#{1,6}\s/.test(line) || /^ {0,3}>/.test(line) || /^ {0,3}(-{3,}|\*{3,})\s*$/.test(line) || /^\s*\|/.test(line);
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    const fence = line.match(FENCE);
    if (fence) {
      const pad = fence[1].length;
      const code = [];
      for (i++; i < lines.length && !new RegExp(`^ *${fence[2][0]}{${fence[2].length},}\\s*$`).test(lines[i]); i++) code.push(dedent(lines[i], pad));
      i++;
      out.push(`<pre><code>${esc(code.join("\n"))}</code></pre>`);
      continue;
    }
    const h = line.match(/^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (h) {
      // `###` in an answer is a sub-heading of it: h4, and h5 below.
      const level = Math.min(6, Math.max(4, h[1].length + 1));
      out.push(`<h${level}>${inline(h[2], anchors)}</h${level}>`);
      i++;
      continue;
    }
    if (/^ {0,3}(-{3,}|\*{3,})\s*$/.test(line)) {
      out.push("<hr>");
      i++;
      continue;
    }
    if (/^\s*\|/.test(line) && i + 1 < lines.length && isTableSep(lines[i + 1])) {
      const head = cells(line);
      const rows = [];
      for (i += 2; i < lines.length && /^\s*\|/.test(lines[i]); i++) rows.push(cells(lines[i]));
      out.push(
        `<div class="faq-table"><table><thead><tr>${head.map((c) => `<th>${inline(c, anchors)}</th>`).join("")}</tr></thead><tbody>${rows
          .map((r) => `<tr>${r.map((c) => `<td>${inline(c, anchors)}</td>`).join("")}</tr>`)
          .join("")}</tbody></table></div>`,
      );
      continue;
    }
    if (/^ {0,3}>/.test(line)) {
      const quote = [];
      for (; i < lines.length && /^ {0,3}>/.test(lines[i]); i++) quote.push(lines[i].replace(/^ {0,3}> ?/, ""));
      out.push(`<blockquote>${markdown(quote.join("\n"), anchors)}</blockquote>`);
      continue;
    }
    const li = line.match(LIST_ITEM);
    if (li) {
      const base = li[1].length;
      const ordered = /\d/.test(li[2]);
      const items = [];
      while (i < lines.length) {
        const m = lines[i].match(LIST_ITEM);
        if (!m || m[1].length !== base || /\d/.test(m[2]) !== ordered) break;
        const content = m[0].length;
        const body = [lines[i].slice(content)];
        let start = ordered ? parseInt(m[2], 10) : null;
        for (i++; i < lines.length; i++) {
          const next = lines[i];
          if (!next.trim()) {
            // A blank line ends the item unless what follows is indented under it.
            const after = lines.slice(i + 1).find((l) => l.trim());
            if (after !== undefined && indentOf(after) > base) {
              body.push("");
              continue;
            }
            break;
          }
          if (indentOf(next) > base) body.push(dedent(next, content));
          else if (!startsBlock(next)) body.push(next.trim()); // a lazy continuation line
          else break;
        }
        items.push({ body: body.join("\n"), start });
      }
      // One paragraph per item is a tight list: no <p> inside.
      const html = items.map(({ body }) => {
        const inner = markdown(body, anchors);
        const tight = /^<p>((?:(?!<p>)[\s\S])*)<\/p>((?:<ul>|<ol)[\s\S]*)?$/.exec(inner);
        return `<li>${tight ? tight[1] + (tight[2] ?? "") : inner}</li>`;
      });
      const first = items[0].start;
      out.push(ordered ? `<ol${first && first !== 1 ? ` start="${first}"` : ""}>${html.join("")}</ol>` : `<ul>${html.join("")}</ul>`);
      continue;
    }
    const para = [];
    for (; i < lines.length && lines[i].trim() && (para.length === 0 || !startsBlock(lines[i])); i++) para.push(lines[i].trim());
    out.push(`<p>${inline(para.join("\n"), anchors).replace(/\n/g, " ")}</p>`);
  }
  return out.join("");
}

// ── The page ───────────────────────────────────────────────────────────────

export function renderFaq(md) {
  const doc = parseFaq(md);
  // FAQ.md's own links between sections use GitHub's anchors; here they go to the topic.
  const anchors = new Map();
  for (const t of doc.topics) {
    anchors.set(githubSlug(t.title), t.id);
    anchors.set(t.id, t.id);
  }
  const groups = doc.groups.length ? doc.groups : [""];
  const groupId = (g) => `g-${slugOf(g)}`;
  const toc = groups
    .map((g) => {
      const topics = doc.topics.filter((t) => t.group === g);
      return `<li><a class="faq-toc-group" href="#${groupId(g)}">${esc(g)}</a><ul>${topics
        .map((t) => `<li><a href="#${t.id}">${inline(t.title, anchors, true)}</a></li>`)
        .join("")}</ul></li>`;
    })
    .join("");
  let count = 0;
  const body = groups
    .map((g) => {
      const topics = doc.topics.filter((t) => t.group === g);
      return `<section class="faq-group" id="${groupId(g)}"><h2>${esc(g)}</h2>${topics
        .map((t) => {
          const intro = t.intro
            ? t.items.length
              ? `<div class="faq-intro faq-unit">${markdown(t.intro, anchors)}</div>`
              : `<details class="faq-q faq-unit" id="${t.id}--about"><summary><span>${inline(t.title, anchors, true)}</span></summary><div class="faq-a">${markdown(t.intro, anchors)}</div></details>`
            : "";
          const items = t.items
            .map((q) => {
              count++;
              return `<details class="faq-q faq-unit" id="${q.id}"><summary><span>${esc(q.question)}</span><a class="faq-link" href="#${q.id}" aria-label="Link to this answer" title="Link to this answer">#</a></summary><div class="faq-a">${markdown(q.answer, anchors)}</div></details>`;
            })
            .join("");
          return `<section class="faq-topic" id="${t.id}"><h3>${inline(t.title, anchors, true)}</h3>${intro}${items}</section>`;
        })
        .join("")}</section>`;
    })
    .join("");
  return { doc, count, toc, body };
}

function main() {
  const md = readFileSync(new URL("FAQ.md", ROOT), "utf8");
  const { doc, count, toc, body } = renderFaq(md);
  if (doc.topics.length < 10 || count < 100) throw new Error(`FAQ.md parsed to only ${doc.topics.length} topics and ${count} questions`);
  const shell = readFileSync(new URL("scripts/site-faq.html", ROOT), "utf8");
  if (!shell.includes("<!--FAQ:TOC-->") || !shell.includes("<!--FAQ:BODY-->")) throw new Error("site-faq.html has no FAQ markers");
  const page = shell.replace("<!--FAQ:TOC-->", () => toc).replace("<!--FAQ:BODY-->", () => body);
  if (process.argv.includes("--check")) {
    console.log(`FAQ: ${doc.groups.length} groups, ${doc.topics.length} topics, ${count} questions, ${Math.round(page.length / 1024)} KiB (not written)`);
    return;
  }
  const out = fileURLToPath(new URL("site/faq/index.html", ROOT));
  mkdirSync(fileURLToPath(new URL("site/faq/", ROOT)), { recursive: true });
  writeFileSync(out, page);
  console.log(`FAQ: ${doc.groups.length} groups, ${doc.topics.length} topics, ${count} questions, ${Math.round(page.length / 1024)} KiB → ${out}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
