import { useCallback, useEffect, useState } from "react";
import { Link2, Tv } from "lucide-react";

import { Button, Input } from "../../components";
import { useTr } from "../../state/lang";
import {
  displayPairCode,
  normalizePairCode,
  pairCodeReady,
  pairResultText,
  playAppApi,
  type PairingState,
} from "./playAppPairingState";

/** How often the list is looked at while this shows (a PS5 showing a code, one connecting). */
const REFRESH_MS = 5000;

/**
 * Play › Play Together › PS5Upload Play: type the code the app shows on the TV, and see (and
 * unpair) the PS5s paired. Hidden on an engine from before PS5Upload Play.
 */
export function PlayAppPairing() {
  const tr = useTr();
  const [state, setState] = useState<PairingState | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ text: string; ok: boolean } | null>(null);

  const refresh = useCallback(async () => {
    setState(await playAppApi.list());
  }, []);

  useEffect(() => {
    let alive = true;
    const look = () =>
      void playAppApi.list().then((s) => {
        if (alive) setState(s);
      });
    look();
    const t = window.setInterval(look, REFRESH_MS);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, []);

  if (!state) return null;

  const pair = async () => {
    if (!pairCodeReady(code)) return;
    setBusy(true);
    const r = await playAppApi.pair(code);
    setBusy(false);
    setNote({ text: pairResultText(tr, r), ok: r.kind === "ok" });
    if (r.kind === "ok") setCode("");
    await refresh();
  };

  return (
    <section
      aria-labelledby="pa-title"
      className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="pa-card"
    >
      <div className="space-y-1">
        <h2 id="pa-title" className="flex items-center gap-2 text-sm font-semibold">
          <Tv size={16} aria-hidden />
          {tr("pa_title", undefined, "PS5Upload Play")}
        </h2>
        <p className="text-sm text-[var(--color-muted)]">
          {tr(
            "pa_intro",
            undefined,
            "Answer friends and run your Play Room from your PS5. Open PS5Upload Play on the PS5, choose Pair, then type the code it shows.",
          )}
        </p>
      </div>

      {state.waiting.length > 0 && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="pa-waiting">
          {tr("pa_waiting", { addr: state.waiting.join(", ") }, `A PS5 at ${state.waiting.join(", ")} is showing a code.`)}
        </p>
      )}

      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void pair();
        }}
      >
        <div className="min-w-0 flex-1 basis-[10rem]">
          <Input
            label={tr("pa_code_label", undefined, "Code on the TV")}
            value={displayPairCode(code)}
            onChange={(e) => setCode(normalizePairCode(e.target.value))}
            placeholder={tr("pa_code_placeholder", undefined, "K7M-2QX")}
            autoComplete="off"
            spellCheck={false}
            disabled={busy || state.lockedS !== null}
            data-testid="pa-code"
          />
        </div>
        <Button
          type="submit"
          size="md"
          variant="primary"
          leftIcon={<Link2 size={14} />}
          disabled={!pairCodeReady(code) || state.lockedS !== null}
          loading={busy}
          data-testid="pa-pair"
        >
          {tr("pa_pair", undefined, "Pair")}
        </Button>
      </form>

      {note && (
        <p
          role="status"
          className={`text-sm ${note.ok ? "text-[var(--color-success)]" : "text-[var(--color-danger)]"}`}
          data-testid="pa-note"
        >
          {note.text}
        </p>
      )}

      {state.apps.length > 0 && (
        <ul className="space-y-2" data-testid="pa-list">
          {state.apps.map((a) => (
            <li key={a.id} className="flex flex-wrap items-center gap-2 text-sm">
              <span className="min-w-0 flex-1 truncate font-medium">{a.name ?? a.console}</span>
              <span className="text-xs text-[var(--color-muted)]">
                {a.connected ? tr("pa_connected", undefined, "Open on the PS5") : tr("pa_not_connected", undefined, "Not open")}
              </span>
              <Button
                size="sm"
                variant="ghost"
                onClick={() =>
                  void playAppApi.unpair(a.id).then(() => {
                    setNote(null);
                    void refresh();
                  })
                }
                data-testid={`pa-unpair-${a.id}`}
              >
                {tr("pa_unpair", undefined, "Unpair")}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
