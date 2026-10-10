import { useState } from "react";
import {
  Check,
  Download,
  HardDrive,
  Link2,
  MonitorSmartphone,
  Moon,
  Pencil,
  Sun,
  X,
} from "lucide-react";

import { Button } from "../../components";
import { LINK_MODES, linkModeFacts } from "../../lib/linkModes";
import { useTr } from "../../state/lang";
import {
  useLinkInstallPrefs,
  type LinkInstallMode,
} from "../../state/linkInstallPrefs";
import {
  linkLabel,
  recentLinksFor,
  useRecentLinksStore,
} from "../../state/recentLinks";

const FIELD = "input";

/** One short fact about a mode ("Keep this computer awake"), with its icon. */
function Fact({ icon: Icon, text }: { icon: typeof Sun; text: string }) {
  return (
    <span className="inline-flex items-center gap-1 rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-[11px] text-[var(--color-muted)]">
      <Icon size={11} aria-hidden />
      {text}
    </span>
  );
}

/**
 * Install from a link: the address, an optional name for it, how it is fetched, and the
 * links used recently on this console.
 *
 * The three ways are buttons that say what each asks of you (who downloads, whether this
 * computer must stay awake, whether it needs disk space), not radio inputs: every console's
 * screens stay mounted, and same-named radio groups in two of them fight over the selection.
 */
export function LinkInstallCard({
  host,
  hostReady,
  checking,
  onInstall,
}: {
  host: string;
  hostReady: boolean;
  /** The link is being checked (the button waits). */
  checking: boolean;
  onInstall: (link: {
    url: string;
    name: string;
    mode: LinkInstallMode;
  }) => void;
}) {
  const tr = useTr();
  const mode = useLinkInstallPrefs((s) => s.modeFor(host));
  const setMode = useLinkInstallPrefs((s) => s.setMode);
  const insecure = useLinkInstallPrefs((s) => s.insecureFor(host));
  const setInsecure = useLinkInstallPrefs((s) => s.setInsecure);
  const recent = useRecentLinksStore((s) => recentLinksFor(s, host));
  const rename = useRecentLinksStore((s) => s.rename);
  const forget = useRecentLinksStore((s) => s.forget);
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [editing, setEditing] = useState<{ url: string; name: string } | null>(
    null,
  );
  const facts = linkModeFacts(mode);

  const modeTitle = (m: LinkInstallMode) =>
    m === "direct"
      ? tr("linkcard.mode.direct", undefined, "The PS5 downloads it")
      : m === "stream"
        ? tr("linkcard.mode.stream", undefined, "Stream through this computer")
        : tr(
            "linkcard.mode.download",
            undefined,
            "Download here first, then install",
          );
  const modeBody = (m: LinkInstallMode) =>
    m === "direct"
      ? tr(
          "linkcard.mode.direct_body",
          undefined,
          "The PS5 fetches the link by itself. It must be able to reach the link, and the address must be short (the app shortens a long one, and then has to stay open).",
        )
      : m === "stream"
        ? tr(
            "linkcard.mode.stream_body",
            undefined,
            "This computer downloads over several connections and passes it straight to the PS5. Usually the fastest, and works when only this computer can reach the link.",
          )
        : tr(
            "linkcard.mode.download_body",
            undefined,
            "This computer saves the whole package, then installs it. Slowest, but a link that expires or drops part-way only costs a retry of the download.",
          );

  return (
    <section
      className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] p-4"
      data-testid="link-install-card"
    >
      <header className="mb-1 flex items-center gap-2">
        <Link2 size={15} aria-hidden />
        <h3 className="text-sm font-semibold">
          {tr("linkcard.title", undefined, "Install from a link")}
        </h3>
      </header>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "linkcard.help",
          undefined,
          "Paste a direct download link to a .pkg. Nothing is copied to the PS5 first, so a 100 GB game needs no spare space there.",
        )}
      </p>

      <div className="grid gap-2 md:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
        <label className="block text-xs text-[var(--color-muted)]">
          {tr("linkcard.url", undefined, "Link")}
          <input
            type="url"
            value={url}
            onChange={(e) => setUrl(e.currentTarget.value)}
            placeholder="https://example.com/game.pkg"
            className={`${FIELD} mt-1`}
            data-testid="link-install-url"
          />
        </label>
        <label className="block text-xs text-[var(--color-muted)]">
          {tr("linkcard.name", undefined, "Name (optional)")}
          <input
            type="text"
            value={name}
            maxLength={80}
            onChange={(e) => setName(e.currentTarget.value)}
            placeholder={tr(
              "linkcard.name_placeholder",
              undefined,
              "e.g. Astro Bot update 1.05",
            )}
            className={`${FIELD} mt-1`}
            data-testid="link-install-name"
          />
        </label>
      </div>
      <p className="mt-1 text-[11px] text-[var(--color-muted)]">
        {tr(
          "linkcard.name_hint",
          undefined,
          "A name is shown in the queue, in Tasks and under Recent links instead of the address, so you can tell links apart and retry the right one.",
        )}
      </p>

      <div className="mb-1 mt-4 text-xs font-medium text-[var(--color-text)]">
        {tr("linkcard.how", undefined, "How should it be fetched?")}
      </div>
      <div
        role="radiogroup"
        aria-label={tr("linkcard.how", undefined, "How should it be fetched?")}
        className="grid gap-2 lg:grid-cols-3"
      >
        {LINK_MODES.map((m) => {
          const f = linkModeFacts(m);
          const on = m === mode;
          return (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={on}
              onClick={() => setMode(host, m)}
              data-testid={`link-mode-${m}`}
              className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] flex flex-col gap-1.5 p-4 text-left transition-colors ${
                on
                  ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                  : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
              }`}
            >
              <span className="flex items-center gap-2 text-sm font-medium text-[var(--color-text)]">
                <span
                  aria-hidden
                  className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-full border ${
                    on
                      ? "border-[color-mix(in_srgb,var(--color-accent)_40%,transparent)] bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                      : "border-[var(--color-muted)]"
                  }`}
                >
                  {on && <Check size={11} />}
                </span>
                {modeTitle(m)}
                {m === "stream" && (
                  <span className="ml-auto shrink-0 whitespace-nowrap rounded-full bg-[var(--color-surface-3)] px-1.5 py-0.5 text-[10px] font-normal text-[var(--color-muted)]">
                    {tr("linkcard.recommended", undefined, "Recommended")}
                  </span>
                )}
              </span>
              <span className="text-xs text-[var(--color-muted)]">
                {modeBody(m)}
              </span>
              <span className="mt-auto flex flex-wrap gap-1 pt-1">
                {f.computerMustStayAwake ? (
                  <Fact
                    icon={Sun}
                    text={tr(
                      "linkcard.fact.awake",
                      undefined,
                      "Keep this computer awake",
                    )}
                  />
                ) : (
                  <Fact
                    icon={Moon}
                    text={tr(
                      "linkcard.fact.can_close",
                      undefined,
                      "You can close the app",
                    )}
                  />
                )}
                {f.needsDiskHere ? (
                  <Fact
                    icon={HardDrive}
                    text={tr(
                      "linkcard.fact.disk",
                      undefined,
                      "Needs disk space here",
                    )}
                  />
                ) : (
                  <Fact
                    icon={HardDrive}
                    text={tr(
                      "linkcard.fact.no_disk",
                      undefined,
                      "No disk space needed",
                    )}
                  />
                )}
                <Fact
                  icon={MonitorSmartphone}
                  text={
                    f.progressHere
                      ? tr(
                          "linkcard.fact.progress_here",
                          undefined,
                          "Progress shown here",
                        )
                      : tr(
                          "linkcard.fact.progress_ps5",
                          undefined,
                          "Progress on the PS5",
                        )
                  }
                />
              </span>
            </button>
          );
        })}
      </div>

      {/* Only where it can do something: in "The PS5 downloads it" the PS5 makes the
          connection and this app has no say in which certificates it accepts. */}
      {facts.certificateCheckApplies && (
        <label className="mt-3 flex items-start gap-2 text-xs">
          <input
            type="checkbox"
            className="mt-0.5"
            checked={insecure}
            onChange={(e) => setInsecure(host, e.currentTarget.checked)}
            data-testid="link-install-insecure"
          />
          <span>
            <span className="text-[var(--color-text)]">
              {tr("linkcard.insecure", undefined, "Skip the certificate check")}
            </span>
            <span className="block text-[var(--color-muted)]">
              {tr(
                "linkcard.insecure_hint",
                undefined,
                "For your own server, or a site whose certificate is out of date. It applies here because this computer makes the connection.",
              )}
            </span>
          </span>
        </label>
      )}

      <div className="mt-3 flex justify-end">
        <Button
          variant="primary"
          size="sm"
          leftIcon={<Download size={14} />}
          loading={checking}
          disabled={!hostReady || !url.trim() || checking}
          onClick={() =>
            onInstall({ url: url.trim(), name: name.trim(), mode })
          }
          data-testid="link-install-go"
        >
          {checking
            ? tr("linkdl.checking", undefined, "Checking link…")
            : tr("linkcard.install", undefined, "Install from link")}
        </Button>
      </div>

      {recent.length > 0 && (
        <div
          className="mt-4 border-t border-[var(--color-border)] pt-3"
          data-testid="recent-links"
        >
          <div className="mb-2 text-xs font-medium text-[var(--color-text)]">
            {tr("linkcard.recent", undefined, "Recent links")}
          </div>
          <ul className="grid gap-1.5">
            {recent.map((l) => (
              <li
                key={l.url}
                className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] flex flex-wrap items-center gap-2 px-2.5 py-1.5"
              >
                {editing?.url === l.url ? (
                  <form
                    className="flex min-w-0 flex-1 items-center gap-2"
                    onSubmit={(e) => {
                      e.preventDefault();
                      rename(host, l.url, editing.name);
                      setEditing(null);
                    }}
                  >
                    <input
                      autoFocus
                      type="text"
                      maxLength={80}
                      value={editing.name}
                      onChange={(e) =>
                        setEditing({ url: l.url, name: e.currentTarget.value })
                      }
                      placeholder={tr(
                        "linkcard.name",
                        undefined,
                        "Name (optional)",
                      )}
                      className={`${FIELD} input-sm`}
                    />
                    <Button type="submit" size="sm" variant="secondary">
                      {tr("save", undefined, "Save")}
                    </Button>
                  </form>
                ) : (
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm text-[var(--color-text)]">
                      {linkLabel(l)}
                    </div>
                    <div
                      className="truncate font-mono text-[11px] text-[var(--color-muted)]"
                      title={l.url}
                    >
                      {l.url}
                    </div>
                  </div>
                )}
                <span className="text-[11px] text-[var(--color-muted)]">
                  {modeTitle(l.mode)}
                </span>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    setUrl(l.url);
                    setName(l.name);
                    setMode(host, l.mode);
                  }}
                >
                  {tr("linkcard.use", undefined, "Use")}
                </Button>
                <button
                  type="button"
                  className="rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
                  aria-label={tr("linkcard.rename", undefined, "Rename")}
                  title={tr("linkcard.rename", undefined, "Rename")}
                  onClick={() => setEditing({ url: l.url, name: l.name })}
                >
                  <Pencil size={13} />
                </button>
                <button
                  type="button"
                  className="rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-bad)]"
                  aria-label={tr(
                    "linkcard.forget",
                    undefined,
                    "Remove from the list",
                  )}
                  title={tr(
                    "linkcard.forget",
                    undefined,
                    "Remove from the list",
                  )}
                  onClick={() => forget(host, l.url)}
                >
                  <X size={13} />
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
