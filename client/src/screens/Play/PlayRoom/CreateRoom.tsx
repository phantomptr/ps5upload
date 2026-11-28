import { useState } from "react";

import { Checkbox, Input, Toggle } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { nameProblem } from "../../../lib/room/names";
import { nameProblemText } from "../../../lib/room/nameText";
import { ownRegion } from "../../../lib/room/regions";
import { approvalFor, visibilityDefaults, type RoomBody, type Visibility } from "../../../lib/room/room";
import { confirmPublic, publicConfirmed, useRoomDraft } from "../../../state/roomDraft";
import { useLangStore, useTr } from "../../../state/lang";
import { ChoiceRow, chatChoices, LanguageSelect, RegionSelect, VisibilityPicker, voiceChoices } from "./roomControls";
import { RulesLink, useRoomNotice } from "./RoomNotice";

type Tr = ReturnType<typeof useTr>;

/** The first time a room goes Public: the notice (once per device), then one confirm (design
 *  5.1). True when it may go Public. */
export async function publicGate(
  tr: Tr,
  ask: () => Promise<boolean>,
  confirm: ReturnType<typeof useConfirm>["confirm"],
): Promise<boolean> {
  if (!(await ask())) return false;
  if (publicConfirmed()) return true;
  const ok = await confirm({
    title: tr("room_public_title", undefined, "Make your room public?"),
    message: tr(
      "room_public_body",
      undefined,
      "Anyone using PS5Upload can see this room and ask to join. You still decide who plays. People from the list never get your PS5 user, and PS & Create stay off for them.",
    ),
    confirmLabel: tr("room_public_go", undefined, "Make public"),
  });
  if (ok) confirmPublic();
  return ok;
}

/**
 * "Create a Play Room" (design 7.1): the room's name, size, who can see it, a password, and
 * more under a fold. Nothing is sent until the room opens (POST /api/join/open `room`); an empty
 * name lets the engine call it "<host>'s room".
 */
export function CreateRoomFields({
  invites,
  savedSize,
  defaultName,
}: {
  /** Invites work: "wifi" keeps Public off (it needs the internet). */
  invites: "anywhere" | "wifi" | null | undefined;
  /** Room size from the saved settings (max_players + 1). */
  savedSize: number;
  /** "<host>'s room", the name the engine gives an empty one. */
  defaultName: string;
}) {
  const tr = useTr();
  const lang = useLangStore((s) => s.lang);
  const draft = useRoomDraft((s) => s.draft);
  const set = useRoomDraft((s) => s.set);
  const notice = useRoomNotice();
  const { confirm, dialog } = useConfirm();
  const [more, setMore] = useState(false);
  const visibility: Visibility = draft.visibility ?? "private";
  const defs = visibilityDefaults(visibility);
  const name = draft.name ?? "";
  const nameIssue = name.trim() ? nameProblem(name, 3, 32) : null;
  const pw = draft.password ?? "";
  const pwShort = pw.length > 0 && pw.length < 4;
  const size = draft.size ?? savedSize;
  const approval = approvalFor(draft.approval ?? { friends: true, password: false }, pw.length >= 4);
  const publicWhy =
    invites === "wifi"
      ? tr("room_public_needs_anywhere", undefined, "Needs Invites work: Anywhere (Settings › Play Room)")
      : invites === null
        ? tr("room_public_needs_update", undefined, "Needs a newer PS5Upload engine")
        : null;

  const pick = async (v: Visibility) => {
    if (v === "public" && !(await publicGate(tr, () => notice.ask("public"), confirm))) return;
    // A new visibility brings its own chat/voice defaults unless the host picked them.
    const d = visibilityDefaults(v);
    set({
      visibility: v,
      region: v === "private" ? draft.region : (draft.region ?? ownRegion()),
      lang: draft.lang ?? lang,
      ...(draft.voice === undefined ? { voice: d.voice } : {}),
      ...(draft.chat === undefined ? { chat: d.chat } : {}),
      ...(draft.start_muted === undefined ? { start_muted: d.startMuted } : {}),
    });
  };

  return (
    <div className="space-y-3" data-testid="room-create">
      <div className="grid gap-3 sm:grid-cols-2">
        <Input
          label={tr("room_name", undefined, "Name")}
          value={name}
          placeholder={defaultName}
          maxLength={40}
          onChange={(e) => set({ name: e.target.value })}
          error={nameIssue ? nameProblemText(tr, nameIssue, true) : undefined}
          data-testid="room-name"
        />
        <ChoiceRow
          label={tr("room_size", undefined, "Size")}
          value={size}
          choices={[2, 3, 4].map((n) => ({ value: n, label: tr("room_size_n", { n }, `${n} players`) }))}
          onPick={(n) => set({ size: n })}
          testId="room-size"
        />
      </div>
      <p className="text-xs text-[var(--color-muted)]" data-testid="room-game-auto">
        {tr("room_game_auto", undefined, "Game: picked up from what your PS5 is playing.")}
      </p>
      <VisibilityPicker value={visibility} onPick={(v) => void pick(v)} publicWhy={publicWhy} />
      <Input
        label={tr("room_password", undefined, "Password")}
        type="password"
        autoComplete="new-password"
        value={pw}
        maxLength={64}
        placeholder={tr("room_password_optional", undefined, "optional")}
        onChange={(e) => set({ password: e.target.value })}
        hint={tr("room_password_hint", undefined, "Needs 4 or more characters. People you invite never need it.")}
        error={pwShort ? tr("room_password_short", undefined, "Use 4 or more characters.") : undefined}
        data-testid="room-password"
      />
      <button
        type="button"
        className="text-sm font-medium text-[var(--color-accent)]"
        aria-expanded={more}
        onClick={() => setMore(!more)}
        data-testid="room-more"
      >
        {more ? "▾ " : "▸ "}
        {tr("room_more", undefined, "More: approvals, chat, voice, language, region, watching")}
      </button>
      {more && (
        <div className="space-y-3 rounded-lg bg-[var(--color-surface)] p-3" data-testid="room-more-body">
          <ApprovalFields
            approval={approval}
            hasPassword={pw.length >= 4}
            onChange={(a) => set({ approval: a })}
          />
          <ChoiceRow label={tr("room_chat", undefined, "Chat")} value={draft.chat ?? defs.chat} choices={chatChoices(tr)} onPick={(c) => set({ chat: c })} testId="room-chat-mode" />
          <ChoiceRow label={tr("room_voice", undefined, "Voice")} value={draft.voice ?? defs.voice} choices={voiceChoices(tr)} onPick={(v) => set({ voice: v })} testId="room-voice-mode" />
          <Toggle
            checked={draft.start_muted ?? defs.startMuted}
            onChange={(on) => set({ start_muted: on })}
            label={tr("room_start_muted", undefined, "New players start muted")}
          />
          <div className="flex flex-wrap gap-3">
            <LanguageSelect value={draft.lang === undefined ? lang : draft.lang} onChange={(l) => set({ lang: l })} />
            <RegionSelect value={draft.region === undefined ? ownRegion() : draft.region} onChange={(r) => set({ region: r })} />
          </div>
          <ChoiceRow
            label={tr("room_watching", undefined, "Watching")}
            value={draft.watchers ?? 2}
            choices={[
              { value: 0, label: tr("room_watching_off", undefined, "Off") },
              { value: 2, label: tr("room_watching_2", undefined, "Up to 2") },
            ]}
            onPick={(w) => set({ watchers: w })}
            testId="room-watching"
          />
          <Toggle
            checked={draft.nearby ?? true}
            onChange={(on) => set({ nearby: on })}
            label={tr("room_nearby", undefined, "Show on this network")}
            hint={tr("room_nearby_hint", undefined, "PS5s and phones on your Wi-Fi can ask without a code.")}
          />
        </div>
      )}
      <RulesLink onShow={notice.show} />
      {notice.dialog}
      {dialog}
    </div>
  );
}

/** Approvals (design 3.1): Ask me, with two opt-ins. */
export function ApprovalFields({
  approval,
  hasPassword,
  onChange,
}: {
  approval: { friends: boolean; password: boolean };
  hasPassword: boolean;
  onChange: (a: { friends: boolean; password: boolean }) => void;
}) {
  const tr = useTr();
  return (
    <div className="space-y-1.5" data-testid="room-approval">
      <p className="text-sm font-medium">{tr("room_approval", undefined, "Ask me before anyone joins, except:")}</p>
      <Checkbox
        checked={approval.friends}
        onChange={(on) => onChange({ ...approval, friends: on })}
        label={tr("room_approval_friends", undefined, "People you've played with get in without asking")}
      />
      <Checkbox
        checked={approval.password && hasPassword}
        disabled={!hasPassword}
        onChange={(on) => onChange({ ...approval, password: on })}
        label={tr("room_approval_password", undefined, "The password lets people in without asking")}
        hint={hasPassword ? undefined : tr("room_approval_password_needs", undefined, "Set a password first.")}
      />
    </div>
  );
}

/** What Create sends: only what the host set, the password only when long enough. */
export function createBody(d: RoomBody): RoomBody {
  const out: RoomBody = { ...d, ...(d.visibility === "public" ? { public_ok: true } : {}) };
  if (!out.name?.trim()) delete out.name;
  else out.name = out.name.trim();
  if (!out.password || out.password.length < 4) delete out.password;
  if (out.approval) out.approval = approvalFor(out.approval, !!out.password);
  return out;
}
