/**
 * FAQ.md as data: groups (`# ` headings after the title), their topics (`## ` headings),
 * and each topic's intro and questions (`**Q: …**` paragraphs, which GitHub renders well).
 *
 * The viewer needs this shape for topic tabs, one-question-at-a-time reading, links to a
 * single question, and a search that returns questions rather than whole sections.
 */

export interface FaqItem {
  id: string;
  question: string;
  /** Markdown of the answer: everything up to the next question or topic. */
  answer: string;
}

export interface FaqTopic {
  id: string;
  title: string;
  /** The `# ` heading this topic sits under; "" before the first one. */
  group: string;
  /** Markdown before the topic's first question (all of it when there are none). */
  intro: string;
  items: FaqItem[];
}

export interface FaqDoc {
  /** Markdown before the first topic, without the document's own `# ` title. */
  intro: string;
  /** Group names in document order (the viewer's tabs). Empty when the document has none. */
  groups: string[];
  topics: FaqTopic[];
}

/** A shareable web-UI link to one answer. `base` is the router basename the build was served
 *  under (VITE_BASE_URL), so a UI hosted at a sub-path links back to itself. */
export function faqItemUrl(origin: string, base: string | undefined, itemId: string): string {
  const prefix = (base || "/").replace(/\/+$/, "");
  return `${origin}${prefix}/faq?item=${encodeURIComponent(itemId)}`;
}

/** A link-safe id from a heading or question. */
export function slugOf(text: string): string {
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

export function parseFaq(md: string): FaqDoc {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const doc: FaqDoc = { intro: "", groups: [], topics: [] };
  let group = "";
  const introLines: string[] = [];
  let topic: FaqTopic | null = null;
  let item: FaqItem | null = null;
  let buf: string[] = [];
  const seen = new Set<string>();
  const unique = (id: string) => {
    let out = id;
    for (let n = 2; seen.has(out); n++) out = `${id}-${n}`;
    seen.add(out);
    return out;
  };
  const flush = () => {
    const text = buf.join("\n").trim();
    buf = [];
    if (item) item.answer = text;
    else if (topic) topic.intro = text;
  };
  // Fenced code can hold lines that look like headings (`# replace eth0 …`).
  let fenced = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*(```|~~~)/.test(line)) fenced = !fenced;
    // A `# ` heading after the first line opens a group. (The first line is the title.)
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
      // Only what precedes the first group is the document's intro.
      if (i > 0 && group === "") introLines.push(line);
      continue;
    }
    if (!fenced && Q_START.test(line)) {
      flush();
      // A question may wrap: it runs to the line that closes its `**`.
      let q = line.replace(Q_START, "");
      while (
        !/\*\*\s*$/.test(q) &&
        i + 1 < lines.length &&
        lines[i + 1].trim() !== ""
      ) {
        q += ` ${lines[++i].trim()}`;
      }
      // Shown as a plain line (a button's label), so inline code marks are dropped.
      const question = q
        .replace(/\*\*\s*$/, "")
        .replace(/`/g, "")
        .trim();
      item = {
        id: unique(`${topic.id}--${slugOf(question)}`),
        question,
        answer: "",
      };
      topic.items.push(item);
      continue;
    }
    buf.push(line);
  }
  flush();
  doc.intro = introLines.join("\n").trim();
  return doc;
}

export interface FaqHit {
  topic: FaqTopic;
  /** null when the match is in a topic's own text rather than a question. */
  item: FaqItem | null;
  score: number;
  /** Plain text around the first match in the body (or the start of it). */
  excerpt: string;
}

/** Markdown reduced to readable text, for matching and excerpts. */
export function plainText(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, (m) => m.replace(/```\w*/g, ""))
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^\s*[-*]\s+/gm, "")
    .replace(/[*_`|>]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function count(haystack: string, needle: string): number {
  let n = 0;
  for (
    let at = haystack.indexOf(needle);
    at !== -1;
    at = haystack.indexOf(needle, at + needle.length)
  )
    n++;
  return n;
}

function excerptAround(text: string, lower: string, words: string[]): string {
  const at =
    words
      .map((w) => lower.indexOf(w))
      .filter((i) => i >= 0)
      .sort((a, b) => a - b)[0] ?? 0;
  const from = Math.max(0, at - 70);
  const to = Math.min(text.length, at + 150);
  return `${from > 0 ? "…" : ""}${text.slice(from, to).trim()}${to < text.length ? "…" : ""}`;
}

/**
 * Questions (and question-less topics) that contain every word of `query`, best first.
 * A word in the question counts for much more than one in the answer, so "fan" finds the
 * question about fans before an answer that mentions one in passing.
 */
export function searchFaq(doc: FaqDoc, query: string): FaqHit[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];
  const hits: FaqHit[] = [];
  const consider = (
    topic: FaqTopic,
    item: FaqItem | null,
    title: string,
    body: string,
  ) => {
    const text = plainText(body);
    const lowerTitle = title.toLowerCase();
    const lowerBody = text.toLowerCase();
    const lowerTopic = topic.title.toLowerCase();
    let score = 0;
    for (const w of words) {
      const inTitle = count(lowerTitle, w);
      const inBody = count(lowerBody, w);
      const inTopic = item ? count(lowerTopic, w) : 0;
      if (inTitle + inBody + inTopic === 0) return;
      score += inTitle * 20 + inTopic * 4 + Math.min(inBody, 5);
    }
    hits.push({
      topic,
      item,
      score,
      excerpt: excerptAround(text, lowerBody, words),
    });
  };
  for (const topic of doc.topics) {
    if (topic.intro)
      consider(topic, null, topic.items.length ? "" : topic.title, topic.intro);
    for (const item of topic.items)
      consider(topic, item, item.question, item.answer);
  }
  return hits.sort((a, b) => b.score - a.score);
}
