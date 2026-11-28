import { useEffect, useState } from "react";
import { Settings2 } from "lucide-react";

import { Button, Input, Toggle } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { historyApi, peopleIn, sizeChoices, type RecentPlayer, type RoomInfo, type Visibility } from "../../../lib/room/room";
import { useTr } from "../../../state/lang";
import type { JoinHostState } from "../playTogetherState";
import { ApprovalFields, publicGate } from "./CreateRoom";
import { ChoiceRow, chatChoices, LanguageSelect, RegionSelect, VisibilityPicker, voiceChoices } from "./roomControls";
import type { RoomActions } from "./useRoomActions";

type Tr = ReturnType<typeof useTr>;

function viaText(tr: Tr, v: RecentPlayer["source"]): string {
  switch (v) {
    case "rooms":
      return tr("room_via_rooms", undefined, "from Rooms");
    case "friends":
      return tr("room_via_friends", undefined, "from Friends");
    case "nearby":
      return tr("room_via_nearby", undefined, "nearby");
    case "again":
      return tr("room_via_again", undefined, "played before");
    case "group":
      return tr("room_via_group", undefined, "same PS5");
    case "invite":
      return tr("room_via_invite", undefined, "invite");
    default:
      return "";
  }
}
export { viaText };

/**
 * Room settings, live (design 3.1, 4): changing who can see the room, its password or size never
 * removes anyone. Each change goes to the engine at once; a refusal is told.
 */
export function RoomSettings({
  state,
  actions,
  askNotice,
}: {
  state: JoinHostState;
  actions: RoomActions;
  askNotice: () => Promise<boolean>;
}) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const room = state.room;
  const [open, setOpen] = useState(false);
  const [pw, setPw] = useState("");
  const inRoom = peopleIn(state.guests);
  const anywhere = state.settings.invites !== "wifi";
  const set = (b: Parameters<RoomActions["update"]>[0]) => void actions.update(b);
  const pick = async (v: Visibility) => {
    if (v === room.visibility) return;
    if (v === "public" && !(await publicGate(tr, askNotice, confirm))) return;
    await actions.update(v === "public" ? { visibility: v, public_ok: true } : { visibility: v });
  };
  if (!room.known) {
    return (
      <p className="text-xs text-[var(--color-muted)]" data-testid="room-settings-old">
        {tr("room_settings_old", undefined, "Room settings need the PS5Upload engine 13.0.0 or newer. Your room is private: only people you invite can join.")}
      </p>
    );
  }
  return (
    <section className="space-y-3" data-testid="room-settings">
      <button
        type="button"
        className="flex items-center gap-1.5 text-sm font-semibold"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        data-testid="room-settings-toggle"
      >
        <Settings2 size={14} aria-hidden />
        {open ? "▾ " : "▸ "}
        {tr("room_settings", undefined, "Room settings")}
      </button>
      {open && (
        <div className="space-y-4 rounded-lg bg-[var(--color-surface)] p-3">
          <p className="text-xs text-[var(--color-muted)]">{tr("room_vis_never_removes", undefined, "Changing who can see your room never removes anyone.")}</p>
          {room.manyWrongPasswords && (
            <p className="text-xs text-[var(--color-warn)]" data-testid="room-many-wrong">
              {tr("room_many_wrong", undefined, "Many wrong passwords. Change it?")}
            </p>
          )}
          <Input
            label={tr("room_name", undefined, "Name")}
            defaultValue={room.name}
            maxLength={40}
            onBlur={(e) => {
              const v = e.target.value.trim();
              if (v && v !== room.name) set({ name: v });
            }}
            data-testid="room-settings-name"
          />
          <ChoiceRow
            label={tr("room_size", undefined, "Size")}
            value={room.size}
            choices={sizeChoices(inRoom).map((c) => ({
              value: c.size,
              label: tr("room_size_n", { n: c.size }, `${c.size} players`),
              disabled: !c.ok,
              title: c.ok ? undefined : tr("room_err_size", { n: inRoom }, `${inRoom} people are in; remove someone first.`),
            }))}
            onPick={(n) => set({ size: n })}
            testId="room-settings-size"
          />
          <VisibilityPicker
            name="room-settings-visibility"
            value={room.visibility}
            onPick={(v) => void pick(v)}
            publicWhy={anywhere ? null : tr("room_public_needs_anywhere", undefined, "Needs Invites work: Anywhere (Settings › Play Room)")}
          />
          <div className="flex flex-wrap items-end gap-2" data-testid="room-settings-password">
            <Input
              label={room.password ? tr("room_password_change", undefined, "New password") : tr("room_password", undefined, "Password")}
              type="password"
              autoComplete="new-password"
              value={pw}
              maxLength={64}
              onChange={(e) => setPw(e.target.value)}
              error={pw.length > 0 && pw.length < 4 ? tr("room_password_short", undefined, "Use 4 or more characters.") : undefined}
              data-testid="room-settings-password-input"
            />
            <Button
              size="sm"
              variant="secondary"
              disabled={pw.length < 4}
              onClick={() => void actions.update({ password: pw }).then((ok) => ok && setPw(""))}
              data-testid="room-settings-password-set"
            >
              {room.password ? tr("room_password_change_go", undefined, "Change") : tr("room_password_set", undefined, "Set password")}
            </Button>
            {room.password && (
              <Button size="sm" variant="ghost" onClick={() => set({ password: "" })} data-testid="room-settings-password-remove">
                {tr("room_password_remove", undefined, "Remove password")}
              </Button>
            )}
          </div>
          <ApprovalFields approval={room.approval} hasPassword={room.password} onChange={(a) => set({ approval: a })} />
          <ChoiceRow label={tr("room_chat", undefined, "Chat")} value={room.chat} choices={chatChoices(tr)} onPick={(c) => void actions.comms({ chat: c })} testId="room-settings-chat" />
          <ChoiceRow label={tr("room_voice", undefined, "Voice")} value={room.voice} choices={voiceChoices(tr)} onPick={(v) => void actions.comms({ voice: v })} testId="room-settings-voice" />
          <Toggle checked={room.startMuted} onChange={(on) => void actions.comms({ start_muted: on })} label={tr("room_start_muted", undefined, "New players start muted")} />
          <div className="flex flex-wrap gap-3">
            <LanguageSelect value={room.lang} onChange={(l) => set({ lang: l })} />
            <RegionSelect value={room.region} onChange={(r) => set({ region: r })} />
          </div>
          <ChoiceRow
            label={tr("room_watching", undefined, "Watching")}
            value={room.watchers > 0 ? 2 : 0}
            choices={[
              { value: 0, label: tr("room_watching_off", undefined, "Off") },
              { value: 2, label: tr("room_watching_2", undefined, "Up to 2") },
            ]}
            onPick={(w) => set({ watchers: w })}
            testId="room-settings-watching"
          />
          <Toggle
            checked={room.nearby}
            onChange={(on) => set({ nearby: on })}
            label={tr("room_nearby", undefined, "Show on this network")}
            hint={tr("room_nearby_hint", undefined, "PS5s and phones on your Wi-Fi can ask without a code.")}
          />
          <PreviewShows room={room} set={set} />
          <Blocked room={room} actions={actions} />
          <RecentPlayers />
        </div>
      )}
      {dialog}
    </section>
  );
}

/** "What people see before joining" (design 6.4). */
function PreviewShows({ room, set: setBody }: { room: RoomInfo; set: (b: Parameters<RoomActions["update"]>[0]) => void }) {
  const tr = useTr();
  const s = room.previewShows;
  // The engine takes the whole set each time (a part left out would go back to its default).
  const set = (b: { preview_shows?: Partial<RoomInfo["previewShows"]>; check_before?: boolean }) =>
    setBody({ preview: { ...s, ...b.preview_shows, check: b.check_before ?? room.checkBefore } });
  return (
    <div className="space-y-1.5" data-testid="room-preview-shows">
      <p className="text-sm font-medium">{tr("room_preview_shows", undefined, "What people see before joining")}</p>
      <Toggle checked={s.names} onChange={(on) => set({ preview_shows: { names: on } })} label={tr("room_preview_names", undefined, "Player names")} />
      <Toggle checked={s.pings} onChange={(on) => set({ preview_shows: { pings: on } })} label={tr("room_preview_pings", undefined, "Their connection quality")} />
      <Toggle checked={s.stream} onChange={(on) => set({ preview_shows: { stream: on } })} label={tr("room_preview_stream", undefined, "Stream quality")} />
      <Toggle checked={s.time} onChange={(on) => set({ preview_shows: { time: on } })} label={tr("room_preview_time", undefined, "How long you've been playing")} />
      <Toggle
        checked={room.checkBefore}
        onChange={(on) => set({ check_before: on })}
        label={tr("room_check_before", undefined, "Let people check their connection before asking")}
        hint={tr(
          "room_check_before_hint",
          undefined,
          "Their app connects to your computer for a few seconds to measure. People you've played with can always do this.",
        )}
      />
    </div>
  );
}

/** Blocked (n): name · tag · when, Unblock. */
function Blocked({ room, actions }: { room: RoomInfo; actions: RoomActions }) {
  const tr = useTr();
  if (!room.blocked.length) return null;
  return (
    <div className="space-y-1" data-testid="room-blocked">
      <p className="text-sm font-medium">{tr("room_blocked", { n: room.blocked.length }, `Blocked (${room.blocked.length})`)}</p>
      <ul className="space-y-1">
        {room.blocked.map((b) => (
          <li key={b.tag} className="flex items-center gap-2 text-sm" data-testid="room-blocked-row">
            <span className="min-w-0 flex-1 truncate">
              {b.name || tr("pt_friend_fallback", undefined, "Friend")} · {b.tag}
            </span>
            <Button size="sm" variant="ghost" onClick={() => void actions.unblock(b.tag)} data-testid="room-unblock">
              {tr("room_unblock", undefined, "Unblock")}
            </Button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Recent players (host only; this computer only, never in bug reports). */
function RecentPlayers() {
  const tr = useTr();
  const [list, setList] = useState<RecentPlayer[] | null>(null);
  useEffect(() => {
    let alive = true;
    void historyApi.read().then((l) => alive && setList(l));
    return () => {
      alive = false;
    };
  }, []);
  if (!list?.length) return null;
  return (
    <div className="space-y-1" data-testid="room-recent">
      <p className="text-sm font-medium">{tr("room_recent", undefined, "Recent players")}</p>
      <ul className="max-h-48 space-y-1 overflow-y-auto">
        {list.map((p) => (
          <li key={p.id} className="flex items-center gap-2 text-sm" data-testid="room-recent-row">
            <span className="min-w-0 flex-1 truncate">
              {p.name || tr("pt_friend_fallback", undefined, "Friend")} · {p.tag}
              <span className="ms-1 text-xs text-[var(--color-muted)]">
                {viaText(tr, p.source)} · {tr("room_recent_minutes", { n: Math.round(p.minutes) }, `${Math.round(p.minutes)} min`)}
              </span>
            </span>
            <Button
              size="sm"
              variant="ghost"
              onClick={() => void historyApi.forget(p.id).then(() => setList((l) => l?.filter((x) => x.id !== p.id) ?? null))}
              data-testid="room-recent-forget"
            >
              {tr("room_forget", undefined, "Forget")}
            </Button>
          </li>
        ))}
      </ul>
    </div>
  );
}
