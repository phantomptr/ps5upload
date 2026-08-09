import { useCallback, useEffect, useMemo, useState } from "react";
import { Globe, RefreshCw, Save, RotateCcw } from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import { fsReadPreview, fsWriteText } from "../../api/ps5";
import {
  PageHeader,
  EmptyState,
  ErrorCard,
  WarningCard,
  Button,
  Badge,
  Input,
  Toggle,
} from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import {
  NANODNS_INI_PATH,
  detectNanoDnsVersion,
  fixNanoDnsYandexDns,
  hasOldYandexDns,
  migrateNanoDns04Config,
  nanoDnsGeneralValue,
  nanoDnsLogPath,
  setNanoDnsGeneralValue,
} from "./nanoDnsConfig";

const YANDEX_DNS_NAME = "Yandex.DNS";

function decodeBase64Text(base64: string): string {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) {
    bytes[index] = binary.charCodeAt(index);
  }
  return new TextDecoder().decode(bytes);
}

/** nanoDNS config editor — reads/writes the on-console nanodns.ini and shows
 *  how to point the PS5's DNS at it. nanoDNS reads its config at startup, so a
 *  save only takes effect after the payload is re-loaded (re-sent). */
export default function NanoDnsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const guard = useStaleHostGuard();
  const [text, setText] = useState<string | null>(null);
  const [original, setOriginal] = useState<string>("");
  const [runtimeLog, setRuntimeLog] = useState<string | null>(null);
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
    setRuntimeLog(null);
    try {
      const r = await fsReadPreview(mgmtAddr(probe.host), NANODNS_INI_PATH);
      if (probe.isStale()) return;
      const content = r.base64 ? decodeBase64Text(r.base64) : "";
      setText(content);
      setOriginal(content);

      // Both nanoDNS 0.3 and 0.4 print their exact version in the first
      // lines of the runtime log. The path itself is configurable, so read it
      // from nanodns.ini rather than assuming the default. A missing/stale log
      // is non-fatal; the config schema provides a weaker fallback signal.
      try {
        const log = await fsReadPreview(
          mgmtAddr(probe.host),
          nanoDnsLogPath(content),
          16 * 1024,
        );
        if (probe.isStale()) return;
        setRuntimeLog(log.base64 ? decodeBase64Text(log.base64) : "");
      } catch {
        if (probe.isStale()) return;
        setRuntimeLog(null);
      }
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
  const version = useMemo(
    () => detectNanoDnsVersion(runtimeLog, text ?? ""),
    [runtimeLog, text],
  );
  const modernMigration = useMemo(
    () => (text === null ? null : migrateNanoDns04Config(text)),
    [text],
  );
  const oldYandexDns = text !== null && hasOldYandexDns(text);
  const quietValue = text === null ? null : nanoDnsGeneralValue(text, "quiet");
  const quietEnabled =
    quietValue !== null && /^[+-]?\d+$/.test(quietValue) && Number(quietValue) !== 0;
  const bind6Value =
    (text === null ? null : nanoDnsGeneralValue(text, "bind6")) ?? "::1";

  const versionLabel = version.version
    ? `nanoDNS v${version.version}`
    : version.source === "config"
      ? "nanoDNS 0.4 config"
      : "nanoDNS ?";

  const versionDetail =
    version.source === "runtime-log" && version.generation === "modern"
      ? tr(
          "nanodns_version_modern",
          undefined,
          "Detected from the runtime log. IPv6 and quiet mode are supported.",
        )
      : version.source === "runtime-log" && version.generation === "legacy"
        ? tr(
            "nanodns_version_legacy",
            undefined,
            "Detected from the runtime log. This 0.3 payload uses IPv4 settings; quiet and bind6 are not applied.",
          )
        : version.source === "config"
          ? tr(
              "nanodns_version_config",
              undefined,
              "The runtime banner was unavailable, but this file contains 0.4 settings. Reload nanoDNS 0.4 to use IPv6 and quiet mode.",
            )
          : tr(
              "nanodns_version_unknown",
              undefined,
              "No runtime banner or 0.4 settings were found. The editor stays in 0.3-compatible mode until nanoDNS 0.4 is detected.",
            );

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
      pushNotification(
        "info",
        withConsolePrefix(
          probe.host,
          tr("nanodns_saved", undefined, "nanoDNS config saved"),
        ),
        {
          body: tr(
            "nanodns_saved_body",
            undefined,
            "Re-load (re-send) nanoDNS from the Payloads tab for the changes to take effect.",
          ),
        },
      );
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setSaving(false);
    }
  }, [host, text, guard, tr]);

  return (
    <div className="flex flex-col gap-5 p-6">
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
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={() => void refresh()}
            disabled={loading || !host?.trim()}
            loading={loading}
          >
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
          title={tr(
            "nanodns_not_loaded_title",
            undefined,
            "nanoDNS isn't set up yet",
          )}
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
            <div className="flex flex-wrap items-center gap-2">
              <Badge
                tone={
                  version.generation === "modern"
                    ? "good"
                    : version.generation === "legacy"
                      ? "neutral"
                      : "warn"
                }
                size="md"
                dot
              >
                {versionLabel}
              </Badge>
              <span className="text-xs text-[var(--color-muted)]">
                {version.source === "runtime-log"
                  ? "nanodns.log"
                  : version.source === "config"
                    ? "nanodns.ini"
                    : "—"}
              </span>
            </div>
            <p className="text-xs text-[var(--color-muted)]">{versionDetail}</p>
          </div>

          {version.generation === "modern" &&
          modernMigration &&
          modernMigration.changes.length > 0 ? (
            <WarningCard
              title={versionLabel}
              detail={tr(
                "nanodns_migrate_body",
                undefined,
                "Keep every custom resolver, override, exception, comment, and path; add only missing nanoDNS 0.4 settings and correct the obsolete Yandex.DNS address. Review the editor, then Save.",
              )}
              action={
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => setText(modernMigration.text)}
                >
                  0.3 → 0.4
                </Button>
              }
            />
          ) : null}

          {version.generation !== "modern" && oldYandexDns && text !== null ? (
            <WarningCard
              title={YANDEX_DNS_NAME}
              detail={tr(
                "nanodns_yandex_body",
                undefined,
                "nanoDNS 0.3 shipped 77.77.88.88 by mistake. Correcting it to 77.88.8.8 is safe for both 0.3 and 0.4 and leaves the rest of the file unchanged.",
              )}
              action={
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => setText(fixNanoDnsYandexDns(text).text)}
                >
                  77.77.88.88 → 77.88.8.8
                </Button>
              }
            />
          ) : null}

          {version.generation === "modern" && text !== null ? (
            <div className="grid gap-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 md:grid-cols-2">
              <Toggle
                checked={quietEnabled}
                disabled={saving}
                onChange={(checked) =>
                  setText(setNanoDnsGeneralValue(text, "quiet", checked ? "1" : "0"))
                }
                label={tr("nanodns_quiet_label", undefined, "Quiet mode")}
                hint={tr(
                  "nanodns_quiet_hint",
                  undefined,
                  "Stops nanoDNS startup popups; logging continues normally.",
                )}
              />
              <Input
                label={tr(
                  "nanodns_bind6_label",
                  undefined,
                  "IPv6 bind address",
                )}
                value={bind6Value}
                disabled={saving}
                spellCheck={false}
                onChange={(event) =>
                  setText(setNanoDnsGeneralValue(text, "bind6", event.currentTarget.value))
                }
                hint={tr(
                  "nanodns_bind6_hint",
                  undefined,
                  "Use ::1 for this console, :: for all IPv6 interfaces, or off to disable IPv6.",
                )}
              />
            </div>
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
