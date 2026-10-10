import { useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { ChevronDown, HelpCircle, Link2, Search, X } from "lucide-react";

import {
  PageHeader,
  ErrorCard,
  EmptyState,
  MarkdownView,
  Button,
} from "../../components";
import { Tabs } from "../../components/Tabs";
import { loadBundledDoc } from "../../lib/bundledDoc";
import { isTauriEnv } from "../../lib/tauriEnv";
import {
  faqItemUrl,
  parseFaq,
  searchFaq,
  slugOf,
  type FaqDoc,
  type FaqItem,
  type FaqTopic,
} from "../../lib/faqDoc";
import { log } from "../../state/logs";
import { useTr } from "../../state/lang";

/**
 * FAQ screen: the bundled FAQ.md as something to look things up in, not one long page.
 *
 *  - Its top-level headings are tabs, each tab's `##` headings a list of topics.
 *  - A topic's questions are closed until asked for, so its list of questions is readable.
 *  - Search returns questions (best match first, with an excerpt), not whole sections.
 *  - `?q=` opens it searching (error messages link here, see lib/installErrorDoc), and
 *    `?topic=` / `?item=` open one topic or one question, so an answer can be linked to.
 */

/** Wraps each query word found in `text` in <mark>. */
function Marked({ text, words }: { text: string; words: string[] }) {
  if (words.length === 0) return <>{text}</>;
  const escaped = words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const parts = text.split(new RegExp(`(${escaped.join("|")})`, "gi"));
  return (
    <>
      {parts.map((p, i) =>
        words.includes(p.toLowerCase()) ? (
          <mark key={i} className="rounded bg-[var(--color-accent-soft)] px-0.5 text-[var(--color-text)]">
            {p}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </>
  );
}

function Question({
  item,
  open,
  onToggle,
  onCopyLink,
  words,
  topicLabel,
  excerpt,
}: {
  item: FaqItem;
  open: boolean;
  onToggle: () => void;
  onCopyLink: (() => void) | null;
  /** Search words to mark in the question (search results only). */
  words?: string[];
  /** Shown above the question in search results. */
  topicLabel?: string;
  excerpt?: string;
}) {
  const tr = useTr();
  return (
    <li
      id={item.id}
      className="surface-panel"
      data-testid="faq-item"
    >
      <button
        type="button"
        aria-expanded={open}
        onClick={onToggle}
        className="flex w-full items-start gap-3 px-4 py-3 text-left"
      >
        <span className="min-w-0 flex-1">
          {topicLabel && (
            <span className="mb-0.5 block text-[11px] uppercase tracking-wide text-[var(--color-muted)]">
              {topicLabel}
            </span>
          )}
          <span className="block text-sm font-medium text-[var(--color-text)]">
            <Marked text={item.question} words={words ?? []} />
          </span>
          {!open && excerpt && (
            <span className="mt-1 block text-xs text-[var(--color-muted)]">
              <Marked text={excerpt} words={words ?? []} />
            </span>
          )}
        </span>
        <ChevronDown
          size={16}
          aria-hidden
          className={`mt-0.5 shrink-0 text-[var(--color-muted)] transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && (
        <div className="border-t border-[var(--color-border)] px-4 pb-3 pt-1">
          <MarkdownView source={item.answer} />
          {onCopyLink && (
            <button
              type="button"
              onClick={onCopyLink}
              className="mt-1 inline-flex items-center gap-1 text-[11px] text-[var(--color-muted)] hover:text-[var(--color-text)]"
            >
              <Link2 size={11} aria-hidden />
              {tr("faq_copy_link", undefined, "Copy a link to this answer")}
            </button>
          )}
        </div>
      )}
    </li>
  );
}

/** The group a topic is shown under: its own, or one tab for a document with none. */
const groupOf = (t: FaqTopic) => t.group || "FAQ";

export default function FAQScreen() {
  const tr = useTr();
  const [raw, setRaw] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [searchParams, setSearchParams] = useSearchParams();
  const [query, setQuery] = useState(() => searchParams.get("q") ?? "");
  const linkedQuery = searchParams.get("q");
  useEffect(() => {
    if (linkedQuery !== null) setQuery(linkedQuery);
  }, [linkedQuery]);
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  // Bumping `loadAttempt` re-runs the load effect — used by the
  // retry button so we can recover without a full window reload
  // (which would dump every other tab's in-flight state too).
  const [loadAttempt, setLoadAttempt] = useState(0);

  useEffect(() => {
    (async () => {
      try {
        setError(null);
        const content = await loadBundledDoc("faq");
        setRaw(content);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        log.error("faq", "failed to load FAQ.md", msg);
        setError(msg);
      }
    })();
  }, [loadAttempt]);

  const doc: FaqDoc | null = useMemo(() => (raw ? parseFaq(raw) : null), [raw]);
  const groups = useMemo(() => {
    if (!doc) return [];
    const names = doc.groups.length ? doc.groups : ["FAQ"];
    return names.map((name) => ({
      name,
      id: slugOf(name),
      topics: doc.topics.filter((t) => groupOf(t) === name),
    }));
  }, [doc]);

  // The topic on show: the linked one, else the linked question's, else the first.
  const linkedItem = searchParams.get("item");
  const topicParam = searchParams.get("topic");
  const topic: FaqTopic | null = useMemo(() => {
    if (!doc) return null;
    const byItem = linkedItem
      ? doc.topics.find((t) => t.items.some((i) => i.id === linkedItem))
      : undefined;
    return (
      doc.topics.find((t) => t.id === topicParam) ?? byItem ?? groups[0]?.topics[0] ?? null
    );
  }, [doc, groups, topicParam, linkedItem]);
  const group = groups.find((g) => topic && g.name === groupOf(topic)) ?? groups[0] ?? null;

  // A linked question opens, once the document is there.
  useEffect(() => {
    if (!doc || !linkedItem) return;
    setOpen((prev) => (prev.has(linkedItem) ? prev : new Set(prev).add(linkedItem)));
    requestAnimationFrame(() =>
      document.getElementById(linkedItem)?.scrollIntoView({ block: "start" }),
    );
  }, [doc, linkedItem]);

  const words = useMemo(() => query.toLowerCase().split(/\s+/).filter(Boolean), [query]);
  const hits = useMemo(() => (doc && words.length ? searchFaq(doc, query) : []), [doc, query, words]);

  const toggle = (id: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  const showTopic = (id: string) => setSearchParams({ topic: id }, { replace: true });
  // A link only means something in the browser build: the desktop and Android apps load from
  // an internal origin nobody else can open. Clipboard also needs a secure context, which the
  // self-hosted web UI often lacks. No button in either case.
  const canCopy =
    !isTauriEnv() && typeof navigator !== "undefined" && !!navigator.clipboard?.writeText;
  const copyLink = (item: FaqItem) => {
    const url = faqItemUrl(window.location.origin, import.meta.env.VITE_BASE_URL, item.id);
    void navigator.clipboard.writeText(url).catch(() => {});
  };
  const allOpen = !!topic && topic.items.length > 0 && topic.items.every((i) => open.has(i.id));
  const setTopicOpen = (on: boolean) =>
    setOpen((prev) => {
      const next = new Set(prev);
      for (const i of topic?.items ?? []) {
        if (on) next.add(i.id);
        else next.delete(i.id);
      }
      return next;
    });

  return (
    <div className="app-page">
      <PageHeader
        icon={HelpCircle}
        title={tr("faq", undefined, "FAQ")}
        description={tr(
          "faq_description_v2",
          undefined,
          "How to set up, transfer, install and fix things. Search for a word or an error code, or pick a section.",
        )}
      />

      <div className="mx-auto max-w-5xl">
        <div className="mb-4 flex items-center gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2">
          <Search size={14} className="shrink-0 text-[var(--color-muted)]" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={tr("faq_search_placeholder", undefined, "Search the FAQ…")}
            className="max-md:min-h-11 flex-1 bg-transparent text-sm outline-none placeholder:text-[var(--color-muted)]"
          />
          {query && (
            <span className="shrink-0 text-xs text-[var(--color-muted)]" data-testid="faq-hit-count">
              {tr("faq_results", { count: hits.length }, "{count} answers")}
            </span>
          )}
          {query && (
            <button
              type="button"
              onClick={() => setQuery("")}
              className="rounded-full p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]"
              aria-label={tr("faq_clear_search", "Clear search")}
            >
              <X size={12} />
            </button>
          )}
        </div>

        {error && (
          <div className="mb-4">
            <ErrorCard
              title={tr("faq_load_error", undefined, "Couldn't load FAQ.md")}
              detail={error}
              action={
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => setLoadAttempt((n) => n + 1)}
                >
                  {/* NOT `try_again` — that key is the hint sentence "Try
                      again in a moment." and rendered as a full sentence,
                      period and all, on this button. This key is the bare
                      imperative and is translated in every locale. */}
                  {tr("errorboundary_try_again", undefined, "Try again")}
                </Button>
              }
            />
          </div>
        )}

        {raw === null && !error && (
          <EmptyState message={tr("faq_loading", undefined, "Loading FAQ…")} />
        )}

        {/* Searching: answers from every section, best first. */}
        {doc && words.length > 0 && hits.length === 0 && (
          <EmptyState
            icon={Search}
            size="hero"
            title={tr("faq_no_matches", undefined, "No matches")}
            message={tr(
              "faq_no_matches_message",
              { query },
              `Nothing in the FAQ matches "${query}". Try a shorter or different phrase.`,
            )}
          />
        )}
        {doc && words.length > 0 && hits.length > 0 && (
          <ul className="grid gap-2" data-testid="faq-results">
            {hits.map((h) =>
              h.item ? (
                <Question
                  key={h.item.id}
                  item={h.item}
                  open={open.has(h.item.id)}
                  onToggle={() => toggle(h.item!.id)}
                  onCopyLink={canCopy ? () => copyLink(h.item!) : null}
                  words={words}
                  topicLabel={h.topic.title}
                  excerpt={h.excerpt}
                />
              ) : (
                <li
                  key={h.topic.id}
                  className="surface-panel"
                >
                  <button
                    type="button"
                    className="block w-full px-4 py-3 text-left"
                    onClick={() => {
                      setQuery("");
                      showTopic(h.topic.id);
                    }}
                  >
                    <span className="mb-0.5 block text-[11px] uppercase tracking-wide text-[var(--color-muted)]">
                      {tr("faq_section", undefined, "Section")}
                    </span>
                    <span className="block text-sm font-medium text-[var(--color-text)]">
                      <Marked text={h.topic.title} words={words} />
                    </span>
                    <span className="mt-1 block text-xs text-[var(--color-muted)]">
                      <Marked text={h.excerpt} words={words} />
                    </span>
                  </button>
                </li>
              ),
            )}
          </ul>
        )}

        {/* Browsing: a tab per group, its topics beside the one on show. */}
        {doc && words.length === 0 && group && topic && (
          <>
            {doc.intro && groups[0]?.topics[0]?.id === topic.id && (
              <div className="mb-2 text-sm text-[var(--color-muted)]">
                <MarkdownView source={doc.intro.replace(/^-{3,}\s*$/gm, "")} />
              </div>
            )}
            {groups.length > 1 && (
              <Tabs
                className="mb-4"
                variant="underline"
                ariaLabel={tr("faq", undefined, "FAQ")}
                value={group.id}
                onChange={(id) => {
                  const g = groups.find((x) => x.id === id);
                  if (g?.topics[0]) showTopic(g.topics[0].id);
                }}
                tabs={groups.map((g) => ({ id: g.id, label: g.name }))}
              />
            )}
            <div className="grid gap-5 md:grid-cols-[14rem_minmax(0,1fr)]">
              <nav
                aria-label={tr("faq_topics", undefined, "Topics")}
                className="flex gap-1 overflow-x-auto md:sticky md:top-2 md:flex-col md:self-start md:overflow-visible"
              >
                {group.topics.map((t) => (
                  <button
                    key={t.id}
                    type="button"
                    aria-current={t.id === topic.id ? "page" : undefined}
                    onClick={() => showTopic(t.id)}
                    className={`shrink-0 rounded-md px-3 py-2 text-left text-sm md:shrink ${
                      t.id === topic.id
                        ? "bg-[var(--color-accent-soft)] font-medium text-[var(--color-text)]"
                        : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
                    }`}
                  >
                    {t.title}
                    {t.items.length > 0 && (
                      <span className="ml-1.5 text-[11px] tabular-nums opacity-70">{t.items.length}</span>
                    )}
                  </button>
                ))}
              </nav>
              <article className="min-w-0">
                <div className="mb-3 flex flex-wrap items-center gap-2">
                  <h2 className="flex-1 text-lg font-semibold text-[var(--color-text)]">{topic.title}</h2>
                  {topic.items.length > 1 && (
                    <Button variant="ghost" size="sm" onClick={() => setTopicOpen(!allOpen)}>
                      {allOpen
                        ? tr("faq_collapse_all", undefined, "Close all")
                        : tr("faq_expand_all", undefined, "Open all")}
                    </Button>
                  )}
                </div>
                {topic.intro && (
                  <div className="mb-3">
                    <MarkdownView source={topic.intro.replace(/^-{3,}\s*$/gm, "")} />
                  </div>
                )}
                <ul className="grid gap-2">
                  {topic.items.map((item) => (
                    <Question
                      key={item.id}
                      item={item}
                      open={open.has(item.id)}
                      onToggle={() => toggle(item.id)}
                      onCopyLink={canCopy ? () => copyLink(item) : null}
                    />
                  ))}
                </ul>
              </article>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
