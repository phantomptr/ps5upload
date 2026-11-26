import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, CheckCircle2, Download, ExternalLink, FileCog, Plus, RefreshCw, Send, Trash2 } from "lucide-react";

import {
  addPayloadSource,
  downloadPayloadSource,
  listPayloadSources,
  PayloadSourceError,
  previewPayloadSource,
  removePayloadSource,
  setPayloadSourceFile,
  type PayloadSource,
  type SourcePreview,
  type SourceProblem,
} from "../../api/payloadSources";
import { sendPayloadChecked } from "../../api/ps5";
import { Badge, Button, Input } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { formatBytes } from "../../lib/format";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { payloadSendRefusalText } from "../../lib/payloadSendRefusal";
import { useConnectionStore } from "../../state/connection";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

/**
 * Payload sources the user adds: a GitHub, Gitea or Forgejo project that publishes payload files.
 * The user picks the file once; the app reads the project's releases when Payloads opens or on
 * Check (the engine keeps what it read for ten minutes), downloads only on Download or Update, and
 * sends only on Send, after a confirm that shows exactly which file goes. Nothing runs in the
 * background. The built-in catalogue below stays a list of links.
 */

/** The text for a source call's failure, in the user's language. `pattern`: the source's file
 *  pattern, for "no matching file". `timeOf` formats a retry time (seconds since the epoch). */
export function sourceProblemText(
  tr: Tr,
  p: SourceProblem,
  pattern = "",
  timeOf: (secs: number) => string = (s) => new Date(s * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
): string {
  switch (p.code) {
    case "url_not_https":
      return tr("psrc_err_not_https", undefined, "Only https:// addresses can be added.");
    case "url_has_credentials":
      return tr("psrc_err_credentials", undefined, "Remove the user name or password from the address.");
    case "url_not_a_project":
      return tr(
        "psrc_err_not_project",
        undefined,
        "That isn't a project address. Paste the project's page, like https://github.com/owner/project.",
      );
    case "url_gitlab":
      return tr("psrc_err_gitlab", undefined, "GitLab projects aren't supported. Use a GitHub, Gitea or Forgejo project.");
    case "url_host_not_allowed":
      return tr(
        "psrc_err_host",
        undefined,
        "That address is this computer or a link-local address. Use the server's address on your network.",
      );
    case "rate_limited":
      return p.retry_at
        ? tr("psrc_err_rate_at", { time: timeOf(p.retry_at) }, `Too many checks for now. Try again after ${timeOf(p.retry_at)}.`)
        : tr("psrc_err_rate", undefined, "Too many checks for now. Try again in a few minutes.");
    case "project_not_found":
      return tr("psrc_err_not_found", undefined, "The project wasn't found, or it isn't public.");
    case "no_matching_file":
      return tr(
        "psrc_err_no_file",
        { pattern },
        `None of the newest releases has a file named like ${pattern}. Change the file if the project renamed it.`,
      );
    case "unreachable":
      return tr("psrc_err_unreachable", undefined, "Couldn't reach the project's site. Check the network and try again.");
    case "checksum_mismatch":
      return tr("psrc_err_checksum", undefined, "The download didn't match its release's checksum, so it was not kept.");
    case "checksum_unreadable":
      return tr("psrc_err_checksum_read", undefined, "The release's checksum file couldn't be read, so nothing was downloaded.");
    case "download_cut_short":
      return tr("psrc_err_cut_short", undefined, "The download stopped before the end. Try again.");
    case "asset_not_payload":
      return tr("psrc_err_not_payload", undefined, "The release's file isn't a payload, so it was not kept.");
    case "redirect_refused":
      return tr("psrc_err_redirect", undefined, "The project's site sent the download to an address that isn't allowed.");
    case "custom_repos_unreadable":
      return tr(
        "payloads_err_custom_repos_unreadable",
        { detail: p.message },
        `Your list of added repos can't be read, so it was left as it is. Fix or delete this file: ${p.message}`,
      );
    default:
      return p.message;
  }
}

function problemOf(e: unknown): SourceProblem {
  if (e instanceof PayloadSourceError) return e.problem;
  return { code: "", message: e instanceof Error ? e.message : String(e) };
}

/** What newer releases may be called, in words. */
export function patternText(tr: Tr, fileName: string, pattern: string): string {
  return pattern && pattern !== fileName
    ? tr("psrc_pattern_version", { pattern }, `Newer releases: a file named like ${pattern} (* is the version).`)
    : tr("psrc_pattern_exact", undefined, "Newer releases: a file with this exact name.");
}

// ── A source's card ──────────────────────────────────────────────────────────────────────────────

export interface SourceCardProps {
  tr: Tr;
  source: PayloadSource;
  busy: "download" | "send" | "remove" | null;
  /** A failure of the last action on this card (download, send). */
  actionError: string | null;
  onDownload: () => void;
  onSend: () => void;
  onChangeFile: () => void;
  onRemove: () => void;
  onOpenProject: () => void;
}

export function SourceCardView({
  tr,
  source: s,
  busy,
  actionError,
  onDownload,
  onSend,
  onChangeFile,
  onRemove,
  onOpenProject,
}: SourceCardProps) {
  const needsFile = !s.file_name;
  const have = s.have;
  const checkError = s.error && !needsFile ? sourceProblemText(tr, s.error, s.file_pattern) : null;
  const canDownload = !needsFile && !!s.latest && (!have || s.update_available);
  return (
    <article data-source-card={s.id} className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <h3 className="min-w-0 break-words text-sm font-semibold">{s.name}</h3>
        <button
          type="button"
          onClick={onOpenProject}
          title={s.project_url}
          className="inline-flex min-w-0 items-center gap-1 break-all text-xs text-[var(--color-muted)] underline hover:text-[var(--color-text)]"
        >
          <ExternalLink size={11} className="shrink-0" />
          <span dir="ltr">{`${s.owner}/${s.repo}`}</span>
        </button>
        {s.update_available && s.latest && (
          <Badge tone="accent" variant="solid">
            {tr("psrc_new_version", { version: s.latest.tag }, `New version available: ${s.latest.tag}`)}
          </Badge>
        )}
      </div>

      {needsFile ? (
        <p className="mt-2 flex items-start gap-1.5 text-xs text-[var(--color-warn)]">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <span>
            {tr(
              "psrc_needs_file",
              undefined,
              "Added before this version: pick the file this source downloads.",
            )}
          </span>
        </p>
      ) : (
        <div className="mt-2 text-xs">
          <div className="break-all">
            <span className="text-[var(--color-muted)]">{tr("psrc_file", undefined, "File")}: </span>
            <span className="font-mono" dir="ltr">{s.file_name}</span>
          </div>
          <div className="mt-0.5 break-words text-[var(--color-muted)]">{patternText(tr, s.file_name, s.file_pattern)}</div>
        </div>
      )}

      {have ? (
        <dl data-testid="psrc-have" className="mt-3 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 text-xs">
          <dt className="text-[var(--color-muted)]">{tr("psrc_have_file", undefined, "Downloaded")}</dt>
          <dd className="break-all font-mono" dir="ltr">{have.file_name}</dd>
          <dt className="text-[var(--color-muted)]">{tr("psrc_release", undefined, "Release")}</dt>
          <dd className="break-all">{have.tag || tr("psrc_release_unknown", undefined, "unknown")}</dd>
          <dt className="text-[var(--color-muted)]">{tr("psrc_size", undefined, "Size")}</dt>
          <dd>{formatBytes(have.size)}</dd>
          <dt className="text-[var(--color-muted)]">{"SHA-256"}</dt>
          <dd className="break-all font-mono" dir="ltr" data-testid="psrc-sha256">{have.sha256}</dd>
        </dl>
      ) : (
        !needsFile && (
          <p className="mt-3 text-xs text-[var(--color-muted)]">
            {s.latest
              ? tr(
                  "psrc_not_downloaded_latest",
                  { version: s.latest.tag, size: formatBytes(s.latest.size) },
                  `Not downloaded yet. Newest: ${s.latest.tag} (${formatBytes(s.latest.size)}).`,
                )
              : tr("psrc_not_downloaded", undefined, "Not downloaded yet.")}
          </p>
        )
      )}
      {have?.checksum === "matched" && (
        <p className="mt-1 inline-flex items-center gap-1 text-xs text-[var(--color-good)]">
          <CheckCircle2 size={11} className="shrink-0" />
          {tr("psrc_checksum_matched", undefined, "Matches the checksum its release lists.")}
        </p>
      )}

      {(checkError || actionError) && (
        <p role="alert" className="mt-2 flex items-start gap-1.5 break-words text-xs text-[var(--color-bad)]">
          <AlertTriangle size={12} className="mt-0.5 shrink-0" />
          <span className="min-w-0">{actionError ?? checkError}</span>
        </p>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        {canDownload && (
          <Button
            variant="primary"
            size="sm"
            leftIcon={<Download size={12} />}
            loading={busy === "download"}
            disabled={busy !== null}
            onClick={onDownload}
          >
            {have ? tr("psrc_update", undefined, "Update") : tr("psrc_download", undefined, "Download")}
          </Button>
        )}
        {have && (
          <Button
            variant={canDownload ? "secondary" : "primary"}
            size="sm"
            leftIcon={<Send size={12} />}
            loading={busy === "send"}
            disabled={busy !== null}
            onClick={onSend}
          >
            {tr("psrc_send", undefined, "Send to PS5")}
          </Button>
        )}
        <Button variant={needsFile ? "primary" : "secondary"} size="sm" leftIcon={<FileCog size={12} />} disabled={busy !== null} onClick={onChangeFile}>
          {needsFile ? tr("psrc_pick_file", undefined, "Pick the file") : tr("psrc_change_file", undefined, "Change file")}
        </Button>
        <Button variant="ghost" size="sm" leftIcon={<Trash2 size={12} />} disabled={busy !== null} onClick={onRemove}>
          {tr("psrc_remove", undefined, "Remove source")}
        </Button>
      </div>
    </article>
  );
}

// ── Adding a source, or picking its file ────────────────────────────────────────────────────────

export interface AddSourceProps {
  tr: Tr;
  /** Changing the file of a source added already: its address is fixed. */
  changing: { name: string } | null;
  url: string;
  preview: SourcePreview | null;
  picked: string;
  busy: boolean;
  error: string | null;
  onUrl: (url: string) => void;
  onLookUp: () => void;
  onPick: (name: string) => void;
  onAdd: () => void;
  onCancel: () => void;
}

export function AddSourceView({ tr, changing, url, preview, picked, busy, error, onUrl, onLookUp, onPick, onAdd, onCancel }: AddSourceProps) {
  const release = preview?.releases[0];
  const file = release?.files.find((f) => f.name === picked);
  return (
    <div data-testid="psrc-add" className="rounded-[var(--radius-card)] border border-[var(--color-accent)] bg-[var(--color-surface)] p-4">
      <h3 className="text-sm font-semibold">
        {changing
          ? tr("psrc_change_title", { name: changing.name }, `Pick the file for ${changing.name}`)
          : tr("psrc_add_title", undefined, "Add a payload source")}
      </h3>
      {!changing && (
        <form
          className="mt-2 flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (!busy && url.trim()) onLookUp();
          }}
        >
          <label className="min-w-[min(100%,16rem)] flex-1 text-xs">
            <span className="mb-1 block text-[var(--color-muted)]">
              {tr("psrc_url_label", undefined, "GitHub, Gitea or Forgejo project address (https)")}
            </span>
            <Input
              type="url"
              dir="ltr"
              value={url}
              placeholder="https://github.com/owner/project"
              onChange={(e) => onUrl(e.target.value)}
              aria-label={tr("psrc_url_label", undefined, "GitHub, Gitea or Forgejo project address (https)")}
            />
          </label>
          <Button type="submit" variant="secondary" size="sm" loading={busy && !preview} disabled={busy || !url.trim()}>
            {tr("psrc_look_up", undefined, "Show its files")}
          </Button>
        </form>
      )}

      {preview && (
        <div className="mt-3">
          <p className="break-words text-xs text-[var(--color-muted)]">
            <span dir="ltr">{preview.project_url}</span>
          </p>
          {!release ? (
            <p className="mt-2 text-xs">
              {tr("psrc_no_files", undefined, "The newest releases of this project have no .elf or .bin file.")}
            </p>
          ) : (
            <fieldset className="mt-2">
              <legend className="text-xs font-medium">
                {tr("psrc_pick_legend", { release: release.tag }, `Pick the payload file (release ${release.tag}):`)}
              </legend>
              <ul className="mt-1 space-y-1">
                {release.files.map((f) => (
                  <li key={f.name}>
                    <label className="flex min-h-[2.25rem] cursor-pointer items-center gap-2 rounded px-1 text-xs hover:bg-[var(--color-surface-2)]">
                      <input
                        type="radio"
                        name="psrc-file"
                        value={f.name}
                        checked={picked === f.name}
                        onChange={() => onPick(f.name)}
                      />
                      <span className="min-w-0 flex-1 break-all font-mono" dir="ltr">{f.name}</span>
                      <span className="shrink-0 text-[var(--color-muted)]">{formatBytes(f.size)}</span>
                    </label>
                  </li>
                ))}
              </ul>
              {file && <p className="mt-2 break-words text-xs text-[var(--color-muted)]">{patternText(tr, file.name, file.pattern)}</p>}
            </fieldset>
          )}
          {!changing && (
            <p role="note" className="mt-3 flex items-start gap-1.5 rounded-[var(--radius-card)] border border-[var(--color-warn)] p-2 text-xs">
              <AlertTriangle size={12} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
              <span>
                {tr(
                  "psrc_trust_note",
                  undefined,
                  "PS5Upload will download and send whatever this project publishes under that name. Only add projects you trust.",
                )}
              </span>
            </p>
          )}
        </div>
      )}

      {error && (
        <p role="alert" className="mt-2 break-words text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        {preview && release && (
          <Button variant="primary" size="sm" loading={busy} disabled={busy || !file} onClick={onAdd}>
            {changing ? tr("psrc_use_file", undefined, "Use this file") : tr("psrc_add_confirm", undefined, "Add source")}
          </Button>
        )}
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {tr("cancel", undefined, "Cancel")}
        </Button>
      </div>
    </div>
  );
}

// ── The section ──────────────────────────────────────────────────────────────────────────────────

type Busy = { id: string; what: "download" | "send" | "remove" } | null;

interface AddState {
  changing: PayloadSource | null;
  url: string;
  preview: SourcePreview | null;
  picked: string;
  busy: boolean;
  error: string | null;
}

export default function PayloadSourcesSection() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const [sources, setSources] = useState<PayloadSource[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [busy, setBusy] = useState<Busy>(null);
  const [cardErrors, setCardErrors] = useState<Record<string, string>>({});
  const [add, setAdd] = useState<AddState | null>(null);
  const { confirm, dialog } = useConfirm();

  const load = useCallback(
    async (check: boolean, force: boolean) => {
      setChecking(check);
      try {
        setSources(await listPayloadSources(check, force));
        setListError(null);
      } catch (e) {
        setListError(sourceProblemText(tr, problemOf(e)));
        setSources((prev) => prev ?? []);
      } finally {
        setChecking(false);
      }
    },
    [tr],
  );

  // Opening Payloads is a check (the engine answers from what it read in the last ten minutes).
  useEffect(() => {
    void load(true, false);
  }, [load]);

  const replace = (s: PayloadSource) => setSources((prev) => (prev ?? []).map((x) => (x.id === s.id ? s : x)));

  const setCardError = (id: string, text: string | null) =>
    setCardErrors((prev) => {
      const next = { ...prev };
      if (text) next[id] = text;
      else delete next[id];
      return next;
    });

  /** The add step's "Show its files": the project's newest release with payload files. */
  async function lookUp() {
    if (!add || add.changing) return;
    setAdd({ ...add, busy: true, error: null, preview: null });
    try {
      const preview = await previewPayloadSource(add.url.trim());
      const first = preview.releases[0]?.files ?? [];
      // A project added already opens on its current file.
      const keep = preview.current_file;
      const picked = first.some((f) => f.name === keep) ? keep : first.length === 1 ? first[0].name : "";
      setAdd((a) => a && { ...a, preview, picked, busy: false });
    } catch (e) {
      setAdd((a) => a && { ...a, busy: false, preview: null, error: sourceProblemText(tr, problemOf(e)) });
    }
  }

  async function confirmAdd() {
    if (!add?.preview || !add.picked) return;
    setAdd({ ...add, busy: true, error: null });
    try {
      const saved = add.changing
        ? await setPayloadSourceFile(add.changing.id, add.picked)
        : await addPayloadSource(add.preview.project_url, add.picked);
      setAdd(null);
      setCardError(saved.id, null);
      // The new file's newest release: from the list the preview just read.
      await load(true, false);
    } catch (e) {
      setAdd((a) => a && { ...a, busy: false, error: sourceProblemText(tr, problemOf(e)) });
    }
  }

  function openChange(s: PayloadSource) {
    const state: AddState = { changing: s, url: s.project_url, preview: null, picked: s.file_name, busy: true, error: null };
    setAdd(state);
    void (async () => {
      try {
        const preview = await previewPayloadSource(s.project_url);
        const first = preview.releases[0]?.files ?? [];
        const picked = first.some((f) => f.name === s.file_name) ? s.file_name : first.length === 1 ? first[0].name : "";
        setAdd((a) => a && a.changing?.id === s.id ? { ...a, preview, picked, busy: false } : a);
      } catch (e) {
        setAdd((a) => a && a.changing?.id === s.id ? { ...a, busy: false, error: sourceProblemText(tr, problemOf(e)) } : a);
      }
    })();
  }

  async function download(s: PayloadSource) {
    setBusy({ id: s.id, what: "download" });
    setCardError(s.id, null);
    try {
      replace(await downloadPayloadSource(s.id));
    } catch (e) {
      setCardError(s.id, sourceProblemText(tr, problemOf(e), s.file_pattern));
    } finally {
      setBusy(null);
    }
  }

  async function send(s: PayloadSource) {
    const have = s.have;
    const h = host?.trim() ?? "";
    if (!have) return;
    if (!h) {
      setCardError(s.id, tr("payloads_no_host", undefined, "No PS5 host set. Connect on the Connection screen first."));
      return;
    }
    const ok = await confirm({
      title: tr("psrc_send_title", { file: have.file_name, host: h }, `Send ${have.file_name} to ${h}?`),
      message: tr(
        "psrc_send_body",
        undefined,
        "This file runs on the console with full access to it. Check that it is the one you expect.",
      ),
      details: [
        `${tr("psrc_release", undefined, "Release")}: ${have.tag || tr("psrc_release_unknown", undefined, "unknown")}`,
        `${tr("psrc_size", undefined, "Size")}: ${formatBytes(have.size)}`,
        `SHA-256: ${have.sha256}`,
      ],
      confirmLabel: tr("psrc_send", undefined, "Send to PS5"),
    });
    if (!ok) return;
    setBusy({ id: s.id, what: "send" });
    setCardError(s.id, null);
    try {
      const { viaPayloadManager } = await sendPayloadChecked(h, have.path);
      pushNotification(
        "success",
        withConsolePrefix(h, tr("psrc_sent", { file: have.file_name }, `Sent ${have.file_name}`)),
        {
          body: viaPayloadManager
            ? tr("psrc_sent_pm", undefined, "Port 9021 didn't answer, so it was launched through Payload Manager. Check the PS5 for its notification.")
            : tr("psrc_sent_body", undefined, "Sent to the loader on port 9021. Check the PS5 for its notification."),
        },
      );
    } catch (e) {
      const why = payloadSendRefusalText(e);
      setCardError(s.id, why);
      pushNotification("error", withConsolePrefix(h, tr("payloads_send_failed", { id: have.file_name }, `Send failed: ${have.file_name}`)), {
        body: why,
        link: "/payloads",
      });
    } finally {
      setBusy(null);
    }
  }

  async function remove(s: PayloadSource) {
    const ok = await confirm({
      title: tr("psrc_remove_title", { name: s.name }, `Remove ${s.name}?`),
      message: tr("psrc_remove_body", undefined, "Its downloaded file is deleted from this computer too."),
      confirmLabel: tr("psrc_remove", undefined, "Remove source"),
      destructive: true,
    });
    if (!ok) return;
    setBusy({ id: s.id, what: "remove" });
    try {
      await removePayloadSource(s.id);
      setSources((prev) => (prev ?? []).filter((x) => x.id !== s.id));
      setCardError(s.id, null);
    } catch (e) {
      setCardError(s.id, sourceProblemText(tr, problemOf(e)));
    } finally {
      setBusy(null);
    }
  }

  const list = sources ?? [];
  return (
    <section data-testid="payload-sources" aria-labelledby="psrc-heading" className="surface-panel p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="psrc-heading" className="text-sm font-semibold">
          {tr("psrc_title", undefined, "Your payload sources")}
        </h2>
        <div className="flex flex-wrap gap-2">
          {list.some((s) => s.file_name) && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              loading={checking}
              disabled={checking}
              onClick={() => void load(true, true)}
            >
              {tr("psrc_check", undefined, "Check")}
            </Button>
          )}
          {!add && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Plus size={12} />}
              onClick={() => setAdd({ changing: null, url: "", preview: null, picked: "", busy: false, error: null })}
            >
              {tr("psrc_add", undefined, "Add a payload source")}
            </Button>
          )}
        </div>
      </div>
      {list.length === 0 && !add && (
        <p className="mt-1 text-xs text-[var(--color-muted)]">
          {tr(
            "psrc_intro",
            undefined,
            "Add a GitHub, Gitea or Forgejo project that publishes payload files. PS5Upload downloads the file you pick only when you press Download, and sends it only when you press Send.",
          )}
        </p>
      )}
      {listError && (
        <p role="alert" className="mt-2 break-words text-xs text-[var(--color-bad)]">
          {listError}
        </p>
      )}
      <div className="mt-3 space-y-3">
        {add && (
          <AddSourceView
            tr={tr}
            changing={add.changing ? { name: add.changing.name } : null}
            url={add.url}
            preview={add.preview}
            picked={add.picked}
            busy={add.busy}
            error={add.error}
            onUrl={(url) => setAdd((a) => a && { ...a, url, preview: null, picked: "", error: null })}
            onLookUp={() => void lookUp()}
            onPick={(picked) => setAdd((a) => a && { ...a, picked })}
            onAdd={() => void confirmAdd()}
            onCancel={() => setAdd(null)}
          />
        )}
        {list.map((s) => (
          <SourceCardView
            key={s.id}
            tr={tr}
            source={s}
            busy={busy?.id === s.id ? busy.what : null}
            actionError={cardErrors[s.id] ?? null}
            onDownload={() => void download(s)}
            onSend={() => void send(s)}
            onChangeFile={() => openChange(s)}
            onRemove={() => void remove(s)}
            onOpenProject={() => void openExternalUrl(s.project_url)}
          />
        ))}
      </div>
      {dialog}
    </section>
  );
}
