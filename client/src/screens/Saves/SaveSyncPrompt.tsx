// "Send <game>'s save to <other console>?" after a game closes (engine save_sync.rs): the engine
// keeps the suggestion, this shows it over every screen. Send copies that one save; Not now puts
// it away until the game closes with a change again. Nothing is ever copied without Send.
import { useContext, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ArrowLeftRight, X } from "lucide-react";

import { Button } from "../../components";
import { saveSyncApi, type SyncView } from "../../api/saveSync";
import { humanizeCaught } from "../../lib/humanizeError";
import { pushNotification } from "../../state/notifications";
import { useRosterStore } from "../../state/roster";
import { useTr } from "../../state/lang";
import { ConsoleNamesContext, saveGameNameNow, useSaveGameName } from "./gameNames";
import { promptFor, suggestionId, type Prompt } from "./saveSync";

/** How often the engine is asked for suggestions; it looks at the consoles once a minute. */
const POLL_MS = 30_000;

export function SyncPromptView({
  prompt,
  busy,
  error,
  onSend,
  onOpen,
  onLater,
}: {
  prompt: Prompt;
  busy: boolean;
  error: string | null;
  onSend: () => void;
  onOpen: () => void;
  onLater: () => void;
}) {
  const tr = useTr();
  const { suggestion: s, from, to } = prompt;
  const game = useSaveGameName(s.title_id).name ?? s.title_id;
  const line =
    s.state === "conflict"
      ? tr("saves_sync_prompt_conflict", { game }, "{game}'s save changed on both PS5s")
      : tr("saves_sync_prompt_send", { game, console: to }, "Send {game}'s save to {console}?");
  const body =
    s.state === "ready"
      ? tr("saves_sync_prompt_ready", { from, to }, "It changed on {from}. The save on {to} is backed up first.")
      : s.state === "waiting"
        ? tr(
            "saves_sync_prompt_waiting",
            { console: to },
            "{console} is off or resting. This is checked again when it answers.",
          )
        : tr("saves_sync_prompt_pick", undefined, "Pick which one to keep in Saves › Save sync.");
  return (
    <div
      role="alertdialog"
      aria-labelledby="save-sync-prompt-title"
      className="glass-float elev-3 pointer-events-auto w-[min(26rem,calc(100vw-2rem))] rounded-[var(--radius-panel)] p-4"
      data-testid="save-sync-prompt"
    >
      <div className="flex items-start gap-3">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <ArrowLeftRight size={18} aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <p id="save-sync-prompt-title" className="text-sm font-semibold">
            {line}
          </p>
          <p className="mt-0.5 text-xs text-[var(--color-muted)]">{body}</p>
          {error ? (
            <p role="alert" className="mt-1 text-xs text-[var(--color-bad)]">
              {error}
            </p>
          ) : null}
        </div>
        <button
          type="button"
          onClick={onLater}
          aria-label={tr("saves_sync_not_now", undefined, "Not now")}
          title={tr("saves_sync_not_now", undefined, "Not now")}
          className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          <X size={14} />
        </button>
      </div>
      <div className="mt-3 flex flex-wrap justify-end gap-2">
        <Button size="sm" variant="ghost" disabled={busy} onClick={onLater} data-testid="save-sync-later">
          {tr("saves_sync_not_now", undefined, "Not now")}
        </Button>
        {s.state === "ready" && (
          <Button size="sm" variant="primary" loading={busy} onClick={onSend} data-testid="save-sync-send">
            {tr("saves_sync_send", undefined, "Send")}
          </Button>
        )}
        {s.state === "conflict" && (
          <Button size="sm" variant="primary" onClick={onOpen}>
            {tr("saves_sync_open", undefined, "Open Save sync")}
          </Button>
        )}
      </div>
    </div>
  );
}

/** Mounted once, over every screen. */
export function SaveSyncPrompt() {
  const tr = useTr();
  const navigate = useNavigate();
  const profiles = useRosterStore((s) => s.profiles);
  const [view, setView] = useState<SyncView | null>(null);
  // Put off here while Not now is on its way to the engine, or when it failed.
  const [later, setLater] = useState<Set<string>>(() => new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const told = useRef<Set<string>>(new Set());
  const names = useContext(ConsoleNamesContext);

  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const v = await saveSyncApi.view();
        if (live) setView(v);
      } catch {
        // No engine, or one before save sync: nothing to show.
        if (live) setView(null);
      }
      if (live) timer = window.setTimeout(() => void poll(), POLL_MS);
    };
    void poll();
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, []);

  const prompt = view ? promptFor(view.suggestions, view.pairs, profiles, later) : null;
  const id = prompt ? suggestionId(prompt.suggestion) : null;
  // A message from the save before is not about this one.
  useEffect(() => {
    setError(null);
    setBusy(false);
  }, [id]);
  // The inbox keeps a line of each new suggestion, for when the app was in the background. It
  // names the game the way the prompt does (the console's name, then the online one).
  useEffect(() => {
    if (!prompt || !id || told.current.has(id) || prompt.suggestion.state !== "ready") return;
    told.current.add(id);
    const to = prompt.to;
    void saveGameNameNow(prompt.suggestion.title_id, names).then((game) =>
      pushNotification(
        "info",
        tr("saves_sync_prompt_send", { game, console: to }, "Send {game}'s save to {console}?"),
        { link: "/saves" },
      ),
    );
  }, [prompt, id, tr, names]);

  if (!prompt || !id) return null;
  const { suggestion: s } = prompt;
  const refresh = async () => {
    try {
      setView(await saveSyncApi.view());
    } catch {
      // Shown again at the next poll.
    }
  };
  const send = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await saveSyncApi.apply(s.pair, [{ key: s.key, direction: s.direction }]);
      const res = r.results[0];
      if (res && !res.ok)
        setError(res.error || tr("saves_sync_failed", undefined, "Couldn't send this save."));
      else await refresh();
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  };
  const putOff = async () => {
    setLater((l) => new Set(l).add(id));
    setError(null);
    try {
      await saveSyncApi.dismiss(s.pair, s.key);
      await refresh();
    } catch {
      // Kept off this screen until the app restarts.
    }
  };
  return (
    <div className="pointer-events-none flex justify-center md:justify-end">
      <SyncPromptView
        prompt={prompt}
        busy={busy}
        error={error}
        onSend={() => void send()}
        onOpen={() => {
          setLater((l) => new Set(l).add(id));
          navigate("/saves");
        }}
        onLater={() => void putOff()}
      />
    </div>
  );
}
