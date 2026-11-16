// The Consoles page (the Connection route, so its links still work): the card of the PS5 picked at
// the top (the console chips), saying how it is, how it is reached and who may use it, and one Add
// a PS5 sheet. Another console's card is shown by picking its chip. A console that is not ready
// shows its setup checklist inside its card; nothing else on the page is setup furniture.

import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { Cable, Plus, Radar } from "lucide-react";

import { Button, PageHeader } from "../../../components";
import { useTr } from "../../../state/lang";
import { useRosterStore } from "../../../state/roster";
import { useTailnet } from "../../../state/useTailnet";
import { getAppVersion } from "../../../lib/appVersion";
import { ConsoleCard, ConsoleMeshPoll } from "./ConsoleCard";
import { AddConsoleSheet } from "./AddConsoleSheet";
import { TailnetFound } from "./TailnetFound";
import { parseInvite } from "./model";

/** A link that opens one console's card (`?console=<id>`, maybe `&devices=1`): the console to pick
 *  at the top first (null when it is already picked, or not on the list), and the address without
 *  the parameters once they are used. `devices` stays while a console is being picked, for the
 *  card that mounts for it. Null when the address has neither. */
export function openCardLink(
  params: URLSearchParams,
  activeId: string | null,
  ids: readonly string[],
): { pick: string | null; next: URLSearchParams } | null {
  const focusId = params.get("console");
  if (!focusId && !params.has("devices")) return null;
  const next = new URLSearchParams(params);
  next.delete("console");
  const pick = focusId && focusId !== activeId && ids.includes(focusId) ? focusId : null;
  if (!pick) next.delete("devices");
  return { pick, next };
}

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
  const invite = useMemo(() => parseInvite(params.get("invite") ?? ""), [params]);
  const focusId = params.get("console");
  const wantDevices = params.get("devices") === "1";
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

  // "Open its card" (`?console=<id>`, from Manage PS5s): the page shows the console picked at the
  // top, so the link picks that console first. The parameter is then dropped, or picking another
  // console at the top would bring this one back. `devices=1` (its device list open) is kept for
  // the card that mounts for it, and dropped once that card has read it.
  useEffect(() => {
    const link = openCardLink(
      params,
      activeId,
      profiles.map((p) => p.id),
    );
    if (!link) return;
    if (link.pick) setActive(link.pick);
    setParams(link.next, { replace: true });
  }, [activeId, profiles, params, setActive, setParams]);

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
      // The new console is picked at the top, so its card is the one on show.
      setActive(id);
      setParams(new URLSearchParams(), { replace: true });
    },
    [setActive, setParams],
  );

  // Only the console picked at the top (the console chips, or the sidebar picker): another
  // console's card is one click on its chip away. The others' device lists are still read, so a
  // device asking to use one of them is prompted for here as well.
  const shown = profiles.find((p) => p.id === activeId) ?? profiles[0] ?? null;

  return (
    <div className="app-page">
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
        {shown ? (
          <ConsoleCard
            // A different console is a new card: nothing opened on one carries over to the next.
            key={shown.id}
            profile={shown}
            appTailnet={appTailnet}
            appVersion={appVersion}
            defaultOpen={wantDevices && (!focusId || focusId === shown.id)}
          />
        ) : null}
        {profiles
          .filter((p) => p.id !== shown?.id)
          .map((p) => (
            <ConsoleMeshPoll key={p.id} host={p.host.trim()} />
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
