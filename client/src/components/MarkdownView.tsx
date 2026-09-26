import { useMemo } from "react";
import { openExternalUrl as openExternal } from "../lib/openExternalUrl";

/**
 * Tiny inline markdown renderer. Scope kept narrow to what FAQ.md and
 * CHANGELOG.md actually use:
 *
 *   # / ## / ### headings
 *   paragraphs (blank line separated)
 *   `inline code`
 *   fenced code blocks ``` ... ```
 *   **bold** and *italic* / _italic_
 *   [link](url)
 *   - or * bullet lists (single-level; not nested)
 *   1. numbered lists (single-level; not nested)
 *   > blockquotes (single line)
 *   --- horizontal rules
 *   | pipe | tables | with a --- separator row
 *
 * Everything else falls through as plain text. We deliberately avoid
 * pulling in react-markdown + remark (~20 KB gzipped) for two files
 * with a predictable grammar — the tradeoff is missed features (nested
 * lists, HTML pass-through), which neither doc uses.
 *
 * Tables ARE handled: FAQ.md is rendered both here and on GitHub, so a
 * table added for the GitHub view would otherwise show up in-app as raw
 * pipe characters.
 *
 * Output is React nodes, so we never inject raw HTML — no XSS surface.
 */

function renderInline(text: string, keyPrefix: string): React.ReactNode[] {
  const pattern =
    /(`[^`]+`)|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\n]+\*)|(_[^_\n]+_)|(\[[^\]]+\]\([^)]+\))/g;
  const out: React.ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  let idx = 0;
  while ((m = pattern.exec(text)) !== null) {
    if (m.index > last) {
      out.push(text.slice(last, m.index));
    }
    const token = m[0];
    const k = `${keyPrefix}-i${idx++}`;
    if (token.startsWith("`") && token.endsWith("`")) {
      out.push(
        <code
          key={k}
          className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface-3)] px-1.5 py-0.5 font-mono text-[0.9em] text-[var(--color-text)]"
        >
          {token.slice(1, -1)}
        </code>,
      );
    } else if (token.startsWith("**") || token.startsWith("__")) {
      out.push(
        <strong key={k} className="font-semibold text-[var(--color-text)]">
          {token.slice(2, -2)}
        </strong>,
      );
    } else if (token.startsWith("*") || token.startsWith("_")) {
      out.push(
        <em key={k} className="italic">
          {token.slice(1, -1)}
        </em>,
      );
    } else if (token.startsWith("[")) {
      const mLink = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(token);
      if (mLink) {
        const [, label, url] = mLink;
        out.push(
          <a
            key={k}
            href={url}
            onClick={(e) => {
              e.preventDefault();
              void openExternal(url);
            }}
            className="font-medium text-[var(--color-accent)] underline decoration-[var(--color-accent)]/30 decoration-[1.5px] underline-offset-[3px] transition-colors hover:text-[var(--color-accent)] hover:decoration-[var(--color-accent)]"
          >
            {label}
          </a>,
        );
      } else {
        out.push(token);
      }
    } else {
      out.push(token);
    }
    last = m.index + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

interface Block {
  kind: "h1" | "h2" | "h3" | "p" | "ul" | "ol" | "hr" | "code" | "blockquote" | "table";
  /** table only: header cells followed by body rows. */
  rows?: string[][];
  content: string;
  items?: string[];
  lang?: string;
}

function parseBlocks(md: string): Block[] {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.startsWith("```")) {
      const lang = line.slice(3).trim();
      const start = i + 1;
      let end = start;
      while (end < lines.length && !lines[end].startsWith("```")) end += 1;
      blocks.push({
        kind: "code",
        content: lines.slice(start, end).join("\n"),
        lang,
      });
      i = end + 1;
      continue;
    }
    if (/^-{3,}\s*$/.test(line) || /^\*{3,}\s*$/.test(line)) {
      blocks.push({ kind: "hr", content: "" });
      i += 1;
      continue;
    }
    if (line.startsWith("# ")) {
      blocks.push({ kind: "h1", content: line.slice(2).trim() });
      i += 1;
      continue;
    }
    if (line.startsWith("## ")) {
      blocks.push({ kind: "h2", content: line.slice(3).trim() });
      i += 1;
      continue;
    }
    if (line.startsWith("### ")) {
      blocks.push({ kind: "h3", content: line.slice(4).trim() });
      i += 1;
      continue;
    }
    if (line.startsWith("> ")) {
      blocks.push({ kind: "blockquote", content: line.slice(2).trim() });
      i += 1;
      continue;
    }
    // Pipe table: a header row, a `|---|---|` separator, then body rows.
    // The separator is what distinguishes a table from a paragraph that
    // merely contains a `|`, so both lines are required before we commit.
    if (line.includes("|") && /^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$/.test(lines[i + 1] ?? "")) {
      const splitRow = (row: string): string[] =>
        row
          .trim()
          .replace(/^\|/, "")
          .replace(/\|$/, "")
          .split("|")
          .map((c) => c.trim());
      const rows: string[][] = [splitRow(line)];
      i += 2; // header + separator
      while (i < lines.length && lines[i].includes("|") && lines[i].trim() !== "") {
        rows.push(splitRow(lines[i]));
        i += 1;
      }
      blocks.push({ kind: "table", content: "", rows });
      continue;
    }
    if (/^\s*[-*+]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*+]\s+/, ""));
        i += 1;
      }
      blocks.push({ kind: "ul", content: "", items });
      continue;
    }
    if (/^\s*\d+\.\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+\.\s+/, ""));
        i += 1;
      }
      blocks.push({ kind: "ol", content: "", items });
      continue;
    }
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const paraLines: string[] = [line];
    i += 1;
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !lines[i].startsWith("#") &&
      !lines[i].startsWith("```") &&
      !/^\s*[-*+]\s+/.test(lines[i]) &&
      !/^\s*\d+\.\s+/.test(lines[i]) &&
      !/^-{3,}\s*$/.test(lines[i])
    ) {
      paraLines.push(lines[i]);
      i += 1;
    }
    blocks.push({ kind: "p", content: paraLines.join(" ") });
  }
  return blocks;
}

export function MarkdownView({ source }: { source: string }) {
  const blocks = useMemo(() => parseBlocks(source), [source]);
  return (
    <div className="max-w-none">
      {blocks.map((b, i) => {
        const key = `b-${i}`;
        switch (b.kind) {
          case "h1":
            return (
              <h1
                key={key}
                className="mb-5 mt-8 text-3xl font-bold tracking-tight text-[var(--color-text)] first:mt-0"
              >
                {renderInline(b.content, key)}
              </h1>
            );
          case "h2":
            return (
              <h2
                key={key}
                className="mb-4 mt-10 border-b-2 border-[var(--color-accent)]/25 pb-2 text-2xl font-semibold tracking-tight text-[var(--color-text)] first:mt-0"
              >
                {renderInline(b.content, key)}
              </h2>
            );
          case "h3":
            return (
              <h3
                key={key}
                className="mb-3 mt-6 text-lg font-semibold tracking-tight text-[var(--color-text)]"
              >
                {renderInline(b.content, key)}
              </h3>
            );
          case "p":
            return (
              <p
                key={key}
                className="mb-4 text-[15px] leading-7 text-[var(--color-text)]"
              >
                {renderInline(b.content, key)}
              </p>
            );
          case "table": {
            const [head, ...body] = b.rows ?? [];
            if (!head) return null;
            return (
              // Horizontal scroll on the wrapper, not the page: a wide
              // support matrix must not make the whole FAQ scroll sideways.
              <div key={key} className="mb-5 overflow-x-auto">
                <table className="w-full border-collapse text-[14px] leading-6">
                  <thead>
                    <tr>
                      {head.map((cell, j) => (
                        <th
                          key={`${key}-h-${j}`}
                          className="border-b border-[var(--color-border)] px-3 py-2 text-left font-semibold"
                        >
                          {renderInline(cell, `${key}-h-${j}`)}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {body.map((row, ri) => (
                      <tr key={`${key}-r-${ri}`}>
                        {row.map((cell, ci) => (
                          <td
                            key={`${key}-r-${ri}-${ci}`}
                            className="border-b border-[var(--color-border)]/50 px-3 py-2 align-top"
                          >
                            {renderInline(cell, `${key}-r-${ri}-${ci}`)}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          }
          case "ul":
            return (
              <ul
                key={key}
                className="mb-5 ml-6 list-disc space-y-2 text-[15px] leading-7 marker:text-[var(--color-accent)]/60"
              >
                {(b.items ?? []).map((item, j) => (
                  <li key={`${key}-${j}`} className="pl-1.5">
                    {renderInline(item, `${key}-${j}`)}
                  </li>
                ))}
              </ul>
            );
          case "ol":
            return (
              <ol
                key={key}
                className="mb-5 ml-6 list-decimal space-y-2 text-[15px] leading-7 marker:font-semibold marker:text-[var(--color-accent)]/80"
              >
                {(b.items ?? []).map((item, j) => (
                  <li key={`${key}-${j}`} className="pl-1.5">
                    {renderInline(item, `${key}-${j}`)}
                  </li>
                ))}
              </ol>
            );
          case "hr":
            return (
              <hr
                key={key}
                className="my-8 border-0 border-t border-[var(--color-border)]"
              />
            );
          case "blockquote":
            return (
              <blockquote
                key={key}
                className="mb-5 rounded-r-md border-l-4 border-[var(--color-accent)] bg-[var(--color-surface-2)] px-4 py-3 text-[15px] italic leading-7 text-[var(--color-muted)]"
              >
                {renderInline(b.content, key)}
              </blockquote>
            );
          case "code":
            return (
              <pre
                key={key}
                className="mb-5 overflow-x-auto rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 font-mono text-[13px] leading-relaxed shadow-sm"
              >
                <code>{b.content}</code>
              </pre>
            );
        }
      })}
    </div>
  );
}
