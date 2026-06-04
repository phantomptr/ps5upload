import { useCallback, useEffect, useState } from "react";
import { Globe, RefreshCw, Save, RotateCcw } from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import { fsReadPreview, fsWriteText } from "../../api/ps5";
import {
  PageHeader,
  EmptyState,
  ErrorCard,
  WarningCard,
  Button,
} from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { pushNotification } from "../../state/notifications";

const NANODNS_INI_PATH = "/data/nanodns/nanodns.ini";

/** nanoDNS config editor — reads/writes the on-console nanodns.ini and shows
 *  how to point the PS5's DNS at it. nanoDNS reads its config at startup, so a
 *  save only takes effect after the payload is re-loaded (re-sent). */
export default function NanoDnsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const guard = useStaleHostGuard();
  const [text, setText] = useState<string | null>(null);
  const [original, setOriginal] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notFound, setNotFound] = useState(false);

  const refresh = useCallback(async () => {
    if (!host?.trim()) return;
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    setNotFound(false);
    try {
      const r = await fsReadPreview(mgmtAddr(probe.host), NANODNS_INI_PATH);
      if (probe.isStale()) return;
      let content = "";
      if (r.base64) {
        const bin = atob(r.base64);
        const bytes = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        content = new TextDecoder().decode(bytes);
      }
      setText(content);
      setOriginal(content);
    } catch {
      if (probe.isStale()) return;
      // Most likely the file/dir doesn't exist yet (nanoDNS not loaded).
      setNotFound(true);
      setText(null);
    } finally {
      setLoading(false);
    }
  }, [host, guard]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const dirty = text !== null && text !== original;

  const save = useCallback(async () => {
    if (!host?.trim() || text === null) return;
    const probe = guard.capture();
    setSaving(true);
    setError(null);
    try {
      const r = await fsWriteText(mgmtAddr(probe.host), NANODNS_INI_PATH, text);
      if (probe.isStale()) return;
      if (!r.ok) {
        setError(r.err ?? "write failed");
        return;
      }
      setOriginal(text);
      pushNotification("info", tr("nanodns_saved", undefined, "nanoDNS config saved"), {
        body: tr(
          "nanodns_saved_body",
          undefined,
          "Re-load (re-send) nanoDNS from the Payloads tab for the changes to take effect.",
        ),
      });
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setSaving(false);
    }
  }, [host, text, guard, tr]);

  return (
    <div className="flex flex-col gap-5 p-5">
      <PageHeader
        icon={Globe}
        title={tr("nanodns_title", undefined, "nanoDNS")}
        loading={loading}
        description={tr(
          "nanodns_subtitle",
          undefined,
          "On-console DNS server. Blocks PlayStation Network / update domains by default, and can redirect any domain to a LAN IP. Edit its config here, then re-load it from Payloads to apply.",
        )}
        right={
          <Button
            variant="secondary"
            onClick={() => void refresh()}
            disabled={loading || !host?.trim()}
          >
            <RefreshCw size={16} className={loading ? "animate-spin" : ""} />
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      {!host?.trim() ? (
        <EmptyState
          icon={Globe}
          size="hero"
          title={tr("nanodns_no_host_title", undefined, "Not connected")}
          message={tr(
            "nanodns_no_host_body",
            undefined,
            "Connect to a PS5 on the Connection tab to edit nanoDNS.",
          )}
        />
      ) : notFound ? (
        <EmptyState
          icon={Globe}
          size="hero"
          title={tr("nanodns_not_loaded_title", undefined, "nanoDNS isn't set up yet")}
          message={tr(
            "nanodns_not_loaded_body",
            undefined,
            "No /data/nanodns/nanodns.ini on the console — load nanoDNS once from the Payloads tab (it writes a default config on first run), then come back to edit it.",
          )}
        />
      ) : (
        <div className="flex flex-col gap-4">
          {/* How to point the PS5 at nanoDNS. */}
          <WarningCard
            title={tr(
              "nanodns_dns_howto_title",
              undefined,
              "Point your PS5's DNS at nanoDNS",
            )}
            detail={tr(
              "nanodns_dns_howto_body",
              undefined,
              "On the PS5: Settings → Network → Set Up Internet → (your connection) → Custom → DNS Settings → Manual. Set Primary DNS to the address from the bind setting in the [general] section of /data/nanodns/nanodns.ini below — for example, with bind=127.0.0.1, set Primary DNS to 127.0.0.1.",
            )}
          />

          {error ? (
            <ErrorCard
              title={tr("nanodns_save_error", undefined, "Couldn't save")}
              detail={error}
            />
          ) : null}

          <div className="flex flex-col gap-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
            <div className="flex items-center justify-between">
              <code className="text-xs text-[var(--color-muted)]">
                {NANODNS_INI_PATH}
              </code>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={!dirty || saving}
                  onClick={() => setText(original)}
                >
                  <RotateCcw size={14} />
                  {tr("nanodns_revert", undefined, "Revert")}
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  loading={saving}
                  disabled={!dirty || saving}
                  onClick={() => void save()}
                >
                  <Save size={14} />
                  {tr("nanodns_save", undefined, "Save")}
                </Button>
              </div>
            </div>
            <textarea
              value={text ?? ""}
              onChange={(e) => setText(e.target.value)}
              spellCheck={false}
              className="h-[28rem] w-full resize-y rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3 font-mono text-xs leading-relaxed outline-none focus:border-[var(--color-accent)]"
            />
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "nanodns_apply_hint",
                undefined,
                "Sections: [general] (log/debug/bind) · [upstream] (server×N, timeout_ms) · [overrides] (mask=IPv4, 0.0.0.0 = block) · [exceptions] (one mask per line). Saving writes the file; re-load nanoDNS from Payloads to apply.",
              )}
            </p>
          </div>
        </div>
      )}
    </div>
  );
}
