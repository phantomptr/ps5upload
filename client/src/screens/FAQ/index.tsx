import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { HelpCircle, Search, X } from "lucide-react";

import {
  PageHeader,
  ErrorCard,
  EmptyState,
  MarkdownView,
  Button,
} from "../../components";
import { log } from "../../state/logs";

/**
 * FAQ screen — renders the bundled FAQ.md. Filter-as-you-type narrows
 * the markdown to sections whose heading OR body matches the query,
 * so a user hunting for "fan threshold" lands on the right section
 * without scrolling the whole doc.
 *
 * The filter operates at the H2 section level because that's the
 * natural "topic" grain in our FAQ (one H2 = one topic). H3 lives
 * inside a topic as a sub-question; H1 is the doc title.
 */

interface Section {
  title: string;
  /** Body markdown below the H2, up to the next H2 or EOF. */
  body: string;
}

/** Split the markdown into H2-rooted sections. The prelude (any
 *  content before the first H2) is emitted as a nameless section so
 *  the title + tagline at the top of FAQ.md still render. */
function splitByH2(md: string): { prelude: string; sections: Section[] } {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const preludeLines: string[] = [];
  const sections: Section[] = [];
  let current: Section | null = null;
  for (const line of lines) {
    if (line.startsWith("## ")) {
      if (current) sections.push(current);
      current = { title: line.slice(3).trim(), body: `${line}\n` };
      continue;
    }
    if (current) {
      current.body += line + "\n";
    } else {
      preludeLines.push(line);
    }
  }
  if (current) sections.push(current);
  return { prelude: preludeLines.join("\n"), sections };
}

export default function FAQScreen() {
  const [raw, setRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    (async () => {
      try {
        const content = await invoke<string>("faq_load");
        setRaw(content);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        log.error("faq", "failed to load FAQ.md", msg);
        setError(msg);
      }
    })();
  }, []);

  const { prelude, sections } = useMemo(
    () => (raw ? splitByH2(raw) : { prelude: "", sections: [] }),
    [raw],
  );

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return sections;
    return sections.filter((s) => s.body.toLowerCase().includes(q));
  }, [sections, query]);

  return (
    <div className="p-6">
      <PageHeader
        icon={HelpCircle}
        title="FAQ"
        description="Everything that falls outside the basic happy path — firmware support, payload recovery, platform quirks, keyboard shortcuts."
      />

      <div className="mx-auto max-w-3xl">
        {/* Search input — filters sections live. Kept sticky-ish
            at the top of the content column so it's always reachable
            no matter how far you've scrolled into the FAQ. */}
        <div className="mb-5 flex items-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2">
          <Search size={14} className="shrink-0 text-[var(--color-muted)]" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search the FAQ…"
            className="flex-1 bg-transparent text-sm outline-none placeholder:text-[var(--color-muted)]"
          />
          {query && (
            <button
              type="button"
              onClick={() => setQuery("")}
              className="rounded p-0.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
              aria-label="Clear search"
            >
              <X size={12} />
            </button>
          )}
          {query && (
            <span className="shrink-0 text-xs text-[var(--color-muted)]">
              {filtered.length} / {sections.length}
            </span>
          )}
        </div>

        {error && (
          <div className="mb-4">
            <ErrorCard
              title="Couldn't load FAQ.md"
              detail={error}
              action={
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => window.location.reload()}
                >
                  Try again
                </Button>
              }
            />
          </div>
        )}

        {raw === null && !error && (
          <EmptyState message="Loading FAQ…" />
        )}

        {raw !== null && filtered.length === 0 && query && (
          <EmptyState
            icon={Search}
            size="hero"
            title="No matches"
            message={`Nothing in the FAQ matches "${query}". Try a shorter or different phrase.`}
          />
        )}

        {raw !== null && (!query || filtered.length > 0) && (
          <article>
            {!query && prelude.trim() && <MarkdownView source={prelude} />}
            {filtered.map((s, i) => (
              <MarkdownView key={`${s.title}-${i}`} source={s.body} />
            ))}
          </article>
        )}
      </div>
    </div>
  );
}
