// Help › Thank you: everyone who supported PS5Upload, by tier. The list comes from
// scripts/supporters.mjs, run on the owner's computer over a Ko-fi export; only names and a tier
// are kept (no amounts, emails or messages), and the tiers are told apart by colour alone.

import { Heart } from "lucide-react";

import { SupportButton } from "../../components/SupportButton";
import supporters from "../../data/supporters.json";
import { useTr } from "../../state/lang";

export type Tier = "champion" | "hero" | "friend" | "supporter";

/** Highest first; each tier's colour (a ring and a soft fill on its names). */
export const TIERS: ReadonlyArray<{ id: Tier; key: string; fallback: string; color: string }> = [
  { id: "champion", key: "thanks_tier_champion", fallback: "Champions", color: "#d4a017" },
  { id: "hero", key: "thanks_tier_hero", fallback: "Heroes", color: "#9b6bdf" },
  { id: "friend", key: "thanks_tier_friend", fallback: "Friends", color: "#3d8fe0" },
  { id: "supporter", key: "thanks_tier_supporter", fallback: "Supporters", color: "#3fae6a" },
];

export interface SupporterList {
  tiers: Partial<Record<Tier, string[]>>;
  anonymous: number;
}

export default function ThanksScreen({ list = supporters as SupporterList }: { list?: SupporterList }) {
  const tr = useTr();
  const shown = TIERS.map((t) => ({ ...t, names: list.tiers[t.id] ?? [] })).filter((t) => t.names.length > 0);
  return (
    <div className="mx-auto max-w-3xl space-y-6 pb-8" data-testid="thanks">
      <header className="flex flex-col items-center gap-3 pt-2 text-center">
        <span className="flex h-12 w-12 items-center justify-center rounded-full bg-[color-mix(in_srgb,#e5484d_14%,transparent)] text-[#e5484d]">
          <Heart size={22} fill="currentColor" aria-hidden />
        </span>
        <h2 className="text-xl font-semibold">{tr("thanks_title", undefined, "Thank you")}</h2>
        <p className="max-w-xl text-sm text-[var(--color-muted)]">
          {tr(
            "thanks_intro",
            undefined,
            "PS5Upload is free, and it keeps getting better because of the people below. Every coffee pays for test hardware, late nights and the next release. Thank you, all of you.",
          )}
        </p>
      </header>

      {shown.map((t) => (
        <section key={t.id} aria-labelledby={`thanks-${t.id}`} data-testid={`thanks-${t.id}`}>
          <h3 id={`thanks-${t.id}`} className="mb-2 flex items-center gap-2 text-sm font-semibold">
            <span className="h-2.5 w-2.5 rounded-full" style={{ background: t.color }} aria-hidden />
            {tr(t.key, undefined, t.fallback)}
          </h3>
          <ul className="flex flex-wrap gap-2">
            {t.names.map((name) => (
              <li
                key={name}
                className="rounded-full border px-3 py-1 text-sm"
                style={{
                  borderColor: `color-mix(in srgb, ${t.color} 55%, transparent)`,
                  background: `color-mix(in srgb, ${t.color} 12%, transparent)`,
                }}
              >
                {name}
              </li>
            ))}
          </ul>
        </section>
      ))}

      {list.anonymous > 0 && (
        <p className="text-center text-sm text-[var(--color-muted)]" data-testid="thanks-anonymous">
          {tr(
            "thanks_anonymous",
            { n: list.anonymous },
            `…and ${list.anonymous} more who chose to stay anonymous. Thank you too.`,
          )}
        </p>
      )}

      <footer className="flex flex-col items-center gap-2 border-t border-[var(--color-border)] pt-5 text-center">
        <p className="text-sm text-[var(--color-muted)]">
          {tr("thanks_cta", undefined, "Want to help too? Your name shows here with the next release.")}
        </p>
        <SupportButton />
      </footer>
    </div>
  );
}
