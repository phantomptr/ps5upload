import { useCallback, useEffect, useState } from "react";
import { Download } from "lucide-react";

import { smpStatus, type SmpStatus } from "../../api/ps5";
import { Button, Spinner, WarningCard } from "../../components";
import { hostOf, mgmtAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { invoke } from "../../lib/invokeLogged";
import { pickPath } from "../../lib/pickPath";
import { krakenRuns, useMounterFor, useMounterStore } from "../../lib/mounter";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";

/** ShadowMount+ on `host`: its status, "checking" while the probe is out, null when it could
 *  not be read. `absent` is true only once it is known not to be running, so a warning keyed
 *  on it never flashes while the probe is still out. `recheck` asks again. */
export function useSmpState(host: string): {
  status: SmpStatus | "checking" | null;
  absent: boolean;
  recheck: () => void;
} {
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const [status, setStatus] = useState<SmpStatus | "checking" | null>("checking");
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!up || !host.trim()) {
      setStatus(null);
      return;
    }
    let cancelled = false;
    setStatus("checking");
    smpStatus(mgmtAddr(host))
      .then((s) => {
        if (!cancelled) {
          setStatus(s);
          useMounterStore.getState().put(host, s);
        }
      })
      .catch(() => {
        if (!cancelled) setStatus(null);
      });
    return () => {
      cancelled = true;
    };
  }, [host, up, tick]);
  const absent =
    status !== null &&
    status !== "checking" &&
    !status.running &&
    !status.errors.some((e) => e.startsWith("proc list:"));
  const recheck = useCallback(() => setTick((t) => t + 1), []);
  return { status, absent, recheck };
}

/** "N games from disk images need ShadowMount+ running", with a way to send it. */
export function SmpNeededWarning({
  host,
  count,
}: {
  host: string;
  /** How many installed games run from a disk image. */
  count: number;
}) {
  const tr = useTr();
  const mounter = useMounterFor(host);
  const [sending, setSending] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  // Kraken Mount runs: it mounts no disk images, and ShadowMount+ must not be started beside it
  // (the engine refuses that send too), so there is nothing to send here.
  if (krakenRuns(mounter)) {
    return (
      <WarningCard
        title={tr(
          "installed_kraken_images_title",
          { count },
          `${count} disk image${count === 1 ? "" : "s"} can't be mounted while Kraken Mount runs`,
        )}
        detail={tr(
          "installed_kraken_images_body",
          undefined,
          "Kraken Mount is running on this PS5. It mounts packages (.pkg) and game folders, not disk images, and ShadowMount+, which mounts images, must not run beside it. To play these, restart the PS5 with ShadowMount+ instead of Kraken Mount, or convert the games to packages or folders.",
        )}
      />
    );
  }
  const send = async () => {
    if (!host.trim()) return;
    setSending(true);
    setMsg(null);
    try {
      // The copy saved in the app, else a file the user picks: the app doesn't download payloads.
      const saved = (await invoke("payloads_local_path", {
        id: "shadowmountplus",
      })) as string | null;
      const path =
        saved ??
        (await pickPath({
          mode: "file",
          title: tr("installed_smp_pick_title", undefined, "Choose the ShadowMount+ file"),
          filters: [
            { name: "Payload", extensions: ["elf", "bin"] },
            { name: "All files", extensions: ["*"] },
          ],
        }));
      if (!path) {
        setMsg(
          tr(
            "installed_smp_no_copy",
            undefined,
            "ShadowMount+ isn't saved in ps5upload. Get it from its project page, then press Send and choose the file.",
          ),
        );
        return;
      }
      const res = (await invoke("payload_send", {
        ip: hostOf(host),
        path,
        port: null,
      })) as { ok?: boolean; error?: string };
      setMsg(
        res?.ok
          ? tr(
              "installed_smp_sent",
              undefined,
              "Sent ShadowMount+ — give it a few seconds, then Refresh.",
            )
          : res?.error ||
              tr(
                "installed_smp_send_failed",
                undefined,
                "Couldn't send ShadowMount+.",
              ),
      );
    } catch (e) {
      setMsg(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setSending(false);
    }
  };
  return (
    <WarningCard
      title={tr(
        "installed_smp_off_title",
        { count },
        `${count} disk image${count === 1 ? "" : "s"} need ShadowMount+ running`,
      )}
      detail={tr(
        "installed_smp_off_body",
        undefined,
        "Disc-image titles are mounted from /mnt/shadowmnt by ShadowMount+. It doesn't look like it's running, so those titles won't mount or launch. Send it to the console, wait a few seconds, then Refresh.",
      )}
      action={
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="primary"
            size="sm"
            disabled={sending}
            onClick={() => void send()}
          >
            {sending ? <Spinner size={14} /> : <Download size={14} />}
            {tr("installed_smp_send", undefined, "Send ShadowMount+")}
          </Button>
          {msg ? (
            <span className="text-xs text-[var(--color-muted)]">{msg}</span>
          ) : null}
        </div>
      }
    />
  );
}
