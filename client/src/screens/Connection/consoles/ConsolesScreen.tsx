// The Consoles page (the Connection route, so its links still work): one card per PS5 on this
// app's list, each saying how it is, how it is reached and who may use it, and one Add a PS5
// sheet. A console that is not ready shows its setup checklist inside its card; nothing else on
// the page is setup furniture.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Cable, Plus, Radar } from "lucide-react";

import { Button, PageHeader } from "../../../components";
import { useTr } from "../../../state/lang";
import { useRosterStore } from "../../../state/roster";
import { useTailnet } from "../../../state/useTailnet";
import { getAppVersion } from "../../../lib/appVersion";
import { ConsoleCard } from "./ConsoleCard";
import { AddConsoleSheet } from "./AddConsoleSheet";
import { TailnetFound } from "./TailnetFound";
import { parseInvite } from "./model";

/** First run: nothing on the list yet. */
export function EmptyConsoles({ onAdd }: { onAdd: () => void }) {
  const tr = useTr();
  return (
    <section
      className="flex flex-col items-center gap-3 rounded-[var(--radius-panel)] border border-dashed border-[var(--glass-edge)] bg-[var(--color-surface)] px-6 py-10 text-center"
      data-testid="consoles-empty"
    >
      <span className="grid h-12 w-12 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
        <Radar size={22} aria-hidden />
      </span>
      <h2 className="text-base font-semibold">{tr("cs_empty_title", undefined, "No PS5 here yet")}</h2>
      <p className="max-w-md text-sm text-[var(--color-muted)]">
        {tr(
          "cs_empty_body",
          undefined,
          "Turn your PS5 on and run your jailbreak's loader. PS5Upload looks for it on your home network and on Tailscale, or adds it from another device's invite.",
        )}
      </p>
      <Button variant="primary" leftIcon={<Plus size={14} />} onClick={onAdd} data-testid="consoles-empty-add">
        {tr("cs_add_title", undefined, "Add a PS5")}
      </Button>
    </section>
  );
}

export default function ConsolesScreen() {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const activeId = useRosterStore((s) => s.active_id);
  const setActive = useRosterStore((s) => s.setActive);
  const { status: appTailnet, setPaused: setTailnetPaused, busy: tailnetBusy } = useTailnet();
  const [params, setParams] = useSearchParams();
  const [appVersion, setAppVersion] = useState<string | null>(null);
  /** This page's own root: another console's hidden copy of the page has the same cards. */
  const root = useRef<HTMLDivElement>(null);
  const invite = useMemo(() => parseInvite(params.get("invite") ?? ""), [params]);
  const focusId = params.get("console");
  const [addOpen, setAddOpen] = useState(() => profiles.length === 0 || !!invite || params.get("add") === "1");

  useEffect(() => {
    getAppVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(null));
  }, []);

  // An invite link opened in the app lands here: the Add sheet shows it.
  const wantAdd = params.get("add") === "1";
  useEffect(() => {
    if (invite || wantAdd) setAddOpen(true);
  }, [invite, wantAdd]);

  // "Open its card" (the console picker): scroll to it.
  useEffect(() => {
    if (!focusId) return;
    const el = root.current?.querySelector(`[data-console="${CSS.escape(focusId)}"]`);
    el?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [focusId, profiles.length]);

  const closeAdd = () => {
    setAddOpen(false);
    if (params.has("invite") || params.has("add")) {
      const next = new URLSearchParams(params);
      next.delete("invite");
      next.delete("add");
      setParams(next, { replace: true });
    }
  };

  const onAdded = useCallback(
    (id: string) => {
      setAddOpen(false);
      setActive(id);
      const next = new URLSearchParams();
      next.set("console", id);
      setParams(next, { replace: true });
    },
    [setActive, setParams],
  );

  // The selected console first: it is the one the other screens act on.
  const ordered = [...profiles].sort((a, b) => Number(b.id === activeId) - Number(a.id === activeId));

  return (
    <div className="app-page" ref={root}>
      <PageHeader
        icon={Cable}
        title={tr("cs_title", undefined, "Consoles")}
        description={tr(
          "cs_description",
          undefined,
          "Your PS5s: how each one is doing, how this device reaches it, and who may use it.",
        )}
        right={
          profiles.length > 0 ? (
            <Button variant="primary" leftIcon={<Plus size={14} />} onClick={() => setAddOpen(true)} data-testid="consoles-add-open">
              {tr("cs_add_title", undefined, "Add a PS5")}
            </Button>
          ) : null
        }
      />
      <div className="grid gap-4" data-testid="consoles-list">
        {profiles.length === 0 ? <EmptyConsoles onAdd={() => setAddOpen(true)} /> : null}
        <TailnetFound
          appTailnet={appTailnet}
          onAdded={onAdded}
          onResume={() => void setTailnetPaused(false)}
          resuming={tailnetBusy}
        />
        {ordered.map((p) => (
          <ConsoleCard
            key={p.id}
            profile={p}
            appTailnet={appTailnet}
            appVersion={appVersion}
            highlight={p.id === focusId}
            defaultOpen={p.id === focusId && params.get("devices") === "1"}
          />
        ))}
      </div>
      <AddConsoleSheet
        open={addOpen}
        onClose={closeAdd}
        initialInvite={invite}
        firstRun={profiles.length === 0}
        onAdded={onAdded}
      />
    </div>
  );
}
