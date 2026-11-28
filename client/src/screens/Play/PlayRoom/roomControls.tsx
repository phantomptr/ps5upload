import { Globe, Lock, Users } from "lucide-react";
import type { ReactNode } from "react";

import { Select } from "../../../components";
import { LANGUAGES } from "../../../state/lang";
import { useTr } from "../../../state/lang";
import type { ChatMode } from "../../../lib/room/chat";
import { REGIONS, regionLabel, type Region } from "../../../lib/room/regions";
import type { Visibility, VoiceMode } from "../../../lib/room/room";

type Tr = ReturnType<typeof useTr>;

/** Visibility in a word and its one-line meaning (design 8). */
export function visibilityText(tr: Tr, v: Visibility): { label: string; hint: string } {
  switch (v) {
    case "private":
      return { label: tr("room_vis_private", undefined, "Private"), hint: tr("room_vis_private_hint", undefined, "Only people you invite") };
    case "friends":
      return {
        label: tr("room_vis_friends", undefined, "Friends"),
        hint: tr("room_vis_friends_hint", undefined, "People you've played with see it's open"),
      };
    case "public":
      return { label: tr("room_vis_public", undefined, "Public"), hint: tr("room_vis_public_hint", undefined, "Anyone can find it in Rooms") };
  }
}

const VIS_ICON = { private: Lock, friends: Users, public: Globe } as const;

/** Private / Friends / Public as three radio rows. `publicWhy`: why Public can't be picked now. */
export function VisibilityPicker({
  value,
  onPick,
  publicWhy,
  name = "room-visibility",
}: {
  value: Visibility;
  onPick: (v: Visibility) => void;
  publicWhy: string | null;
  name?: string;
}) {
  const tr = useTr();
  return (
    <fieldset className="space-y-1.5" data-testid="room-visibility">
      <legend className="mb-1 text-sm font-medium">{tr("room_who_sees", undefined, "Who can see it")}</legend>
      {(["private", "friends", "public"] as const).map((v) => {
        const t = visibilityText(tr, v);
        const Icon = VIS_ICON[v];
        const off = v === "public" && !!publicWhy;
        return (
          <label
            key={v}
            className={`flex cursor-pointer items-start gap-2 rounded-lg border px-3 py-2 text-sm ${
              value === v ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]" : "border-[var(--color-border)]"
            } ${off ? "cursor-not-allowed opacity-60" : ""}`}
            data-testid={`room-vis-${v}`}
          >
            <input type="radio" name={name} className="mt-1" checked={value === v} disabled={off} onChange={() => onPick(v)} />
            <Icon size={14} className="mt-0.5 shrink-0" aria-hidden />
            <span className="min-w-0">
              <span className="font-medium">{t.label}</span>
              <span className="block text-xs text-[var(--color-muted)]">{off ? publicWhy : t.hint}</span>
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

/** A small labelled row of choices (Size, Chat, Voice). */
export function ChoiceRow<T extends string | number>({
  label,
  value,
  choices,
  onPick,
  testId,
}: {
  label: string;
  value: T;
  choices: Array<{ value: T; label: string; disabled?: boolean; title?: string }>;
  onPick: (v: T) => void;
  testId: string;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm" role="radiogroup" aria-label={label} data-testid={testId}>
      <span className="min-w-[5rem] text-[var(--color-muted)]">{label}</span>
      {choices.map((c) => (
        <button
          key={String(c.value)}
          type="button"
          role="radio"
          aria-checked={value === c.value}
          disabled={c.disabled}
          title={c.title}
          onClick={() => onPick(c.value)}
          className={`rounded-full border px-3 py-1 text-xs font-medium disabled:cursor-not-allowed disabled:opacity-40 ${
            value === c.value
              ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-[var(--color-on-accent,#fff)]"
              : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]"
          }`}
          data-testid={`${testId}-${c.value}`}
        >
          {c.label}
        </button>
      ))}
    </div>
  );
}

export function chatChoices(tr: Tr): Array<{ value: ChatMode; label: string }> {
  return [
    { value: "off", label: tr("room_chat_off", undefined, "Off") },
    { value: "quick", label: tr("room_chat_quick", undefined, "Quick messages only") },
    { value: "on", label: tr("room_chat_on", undefined, "On") },
  ];
}

export function voiceChoices(tr: Tr): Array<{ value: VoiceMode; label: string }> {
  return [
    { value: "off", label: tr("room_voice_off", undefined, "Off") },
    { value: "ptt", label: tr("room_voice_ptt", undefined, "Push-to-talk only") },
    { value: "open", label: tr("room_voice_open", undefined, "Open mic allowed") },
  ];
}

/** Language: one of the app's 21 or Any. */
export function LanguageSelect({ value, onChange }: { value: string | null; onChange: (v: string | null) => void }) {
  const tr = useTr();
  return (
    <Select
      label={tr("room_language", undefined, "Language")}
      block={false}
      className="w-44"
      value={value ?? ""}
      onChange={(e) => onChange(e.target.value || null)}
      options={[{ value: "", label: tr("room_language_any", undefined, "Any") }, ...LANGUAGES.map((l) => ({ value: l.code, label: l.label }))]}
      data-testid="room-lang"
    />
  );
}

/** Region: one of 12, or hidden. */
export function RegionSelect({ value, onChange }: { value: Region | null; onChange: (v: Region | null) => void }) {
  const tr = useTr();
  return (
    <Select
      label={tr("room_region", undefined, "Region")}
      block={false}
      className="w-56"
      value={value ?? ""}
      onChange={(e) => onChange((e.target.value || null) as Region | null)}
      options={[{ value: "", label: tr("room_region_hidden", undefined, "Don't show") }, ...REGIONS.map((r) => ({ value: r, label: regionLabel(tr, r) }))]}
      data-testid="room-region"
    />
  );
}

/** A chip in the room header. */
export function Chip({ children, tone = "plain", testId }: { children: ReactNode; tone?: "plain" | "warn" | "good"; testId?: string }) {
  const cls =
    tone === "warn"
      ? "bg-[var(--color-warn-soft)] text-[var(--color-warn)]"
      : tone === "good"
        ? "bg-[var(--color-good-soft)] text-[var(--color-good)]"
        : "bg-[var(--color-surface-3)]";
  return (
    <span className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium ${cls}`} data-testid={testId}>
      {children}
    </span>
  );
}

/** "Not listed: <why>" (design 3.3), with what fixes it. */
export function notListedText(tr: Tr, why: string | null): string {
  switch (why) {
    case "internet_off":
      return tr("room_listed_internet_off", undefined, "Not listed: turn on Invites work: Anywhere");
    case "hub":
      return tr("room_listed_hub", undefined, "Not listed: can't reach the rooms service");
    case "name":
      return tr("room_listed_name", undefined, "Not listed: pick another name");
    case "busy":
      return tr("room_listed_busy", undefined, "Not listed: too many rooms right now, try again later");
    case "limit":
      return tr("room_listed_limit", undefined, "Not listed: your network has too many rooms open");
    case "old_hub":
      return tr("room_listed_old_hub", undefined, "Not listed: this rooms service has no rooms (self-hosted)");
    default:
      return tr("room_listed_other", undefined, "Not listed right now");
  }
}
