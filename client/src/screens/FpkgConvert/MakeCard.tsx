import { Folder, Package, HardDrive } from "lucide-react";

import { Badge, Card } from "../../components";
import type { MakeKind } from "../../state/convertPrefs";
import { useTr } from "../../state/lang";

/** Said on ② and ③ while a game is being made: what is set there now is for what comes next. */
export function NextGameNote() {
  const tr = useTr();
  return (
    <div className="text-xs text-[var(--color-accent)]" data-testid="next-game-note">
      {tr(
        "fpkg.next_note",
        undefined,
        "A game is being made. What you set here is for the next game you add under ①; the one being made keeps its own.",
      )}
    </div>
  );
}

/**
 * ② What to make: a package the PS5 installs, a game image ShadowMount+ mounts, or (#469) the
 * game's files back as a plain folder. They are explained side by side, and once a game is
 * checked the package is marked as the ordinary choice (a PS5 fake package plays on every
 * jailbroken firmware, up to 13.60). The folder is never marked: it is a way back, not a way in.
 * For a PS4 game the package is a PS4 fake package, which the engine writes from the game.
 */
export function MakeCard({
  make,
  onMake,
  checked,
  archive = false,
  locked = false,
  forNext = false,
  ps4 = false,
  kraken = false,
}: {
  make: MakeKind;
  onMake: (make: MakeKind) => void;
  /** A game has been checked: the recommendation is about it. */
  checked: boolean;
  /** The source is an archive: it is looked at only once a run unpacks it. */
  archive?: boolean;
  locked?: boolean;
  /** A game is being made: this card sets what the next game added becomes (#669). */
  forNext?: boolean;
  /** The checked game is a PS4 game: the package is a PS4 one. */
  ps4?: boolean;
  /** Kraken Mount runs on the console (and ShadowMount+ does not): it mounts a PS5 package
   *  from a drive without installing it, and mounts no images. */
  kraken?: boolean;
}) {
  const tr = useTr();
  const recommended: "pkg" | null = checked ? "pkg" : null;
  const tiles: { id: MakeKind; icon: typeof Package; title: string; body: string }[] = [
    ps4
      ? {
          id: "pkg",
          icon: Package,
          title: tr("fpkg.make.ps4_pkg_title_v2", undefined, "A PS4 fake package (FPKG, .pkg)"),
          body: tr(
            "fpkg.make.ps4_pkg_body",
            undefined,
            "A PS4 fake package the PS5 installs like a game from the store, streamed from here or uploaded first. It is made with the usual fake keys and read back before it is kept.",
          ),
        }
      : {
          id: "pkg",
          icon: Package,
          title: tr("fpkg.make.pkg_title_v2", undefined, "A fake package (FPKG, .pkg)"),
          body: kraken
            ? tr(
                "fpkg.make.pkg_body_kraken_v4",
                undefined,
                "A fake package (FPKG). Kraken Mount, running on this PS5, mounts it from a drive's homebrew folder and shows it on the home screen, with nothing installed. It can also be installed like a game from the store.",
              )
            : tr(
                "fpkg.make.pkg_body_v6",
                undefined,
                "A fake package (FPKG). There are two ways to play a PS5 one: install it like a game from the store (streamed from here or uploaded first), or mount it with Kraken Mount from a folder it scans. Installing on firmware 11.61 to 13.60 needs the latest kstuff (with fake-package support up to 13.60). PS4 packages are installed, and play on any firmware.",
              ),
        },
    {
      id: "image",
      icon: HardDrive,
      title: tr("fpkg.make.image_title", undefined, "A game image"),
      body: kraken
        ? tr(
            "fpkg.make.image_body_kraken",
            undefined,
            "One file on a PS5 drive that ShadowMount+ mounts. Kraken Mount, which runs on this PS5, does not mount images: make a package instead.",
          )
        : tr(
            "fpkg.make.image_body",
            undefined,
            "One file on a PS5 drive that ShadowMount+ mounts and shows on the home screen. Nothing is installed, and it plays on firmware where a PS5 package will not.",
          ),
    },
    {
      id: "folder",
      icon: Folder,
      title: tr("fpkg.make.folder_title", undefined, "A game folder"),
      body: tr(
        "fpkg.make.folder_body",
        undefined,
        "The game's files as a plain folder on this computer, from a game image or a package made with the debug key. Edit it, keep it, or upload it like any folder.",
      ),
    },
  ];
  // Compact (the user asked for a denser Workshop): the tiles are one line each, and only the
  // chosen one's explanation is shown under them; each tile's own is its tooltip.
  const chosen = tiles.find((t) => t.id === make) ?? tiles[0];
  return (
    <Card padded={false} className="p-3.5 sm:p-4">
      <div className="flex flex-col gap-2 text-sm">
        <div className="font-medium">{tr("fpkg.card.make", undefined, "② What to make")}</div>
        {forNext && <NextGameNote />}
        <div className="grid grid-cols-1 gap-1.5 sm:grid-cols-3 sm:gap-2" role="radiogroup" data-testid="make-choice">
          {tiles.map((t) => {
            const on = make === t.id;
            const Icon = t.icon;
            return (
              <button
                key={t.id}
                type="button"
                role="radio"
                aria-checked={on}
                disabled={locked}
                onClick={() => onMake(t.id)}
                title={t.body}
                className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] px-2.5 py-2 text-start disabled:opacity-60 sm:px-3 ${
                  on
                    ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                    : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
                }`}
              >
                <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <Icon size={15} className="hidden shrink-0 text-[var(--color-muted)] sm:block" aria-hidden />
                  <span className="text-xs font-semibold text-[var(--color-text)] sm:text-sm">{t.title}</span>
                  {recommended === t.id && (
                    <Badge tone="accent" size="sm">
                      {tr("fpkg.make.recommended", undefined, "Recommended")}
                    </Badge>
                  )}
                </div>
              </button>
            );
          })}
        </div>
        <div className="text-xs leading-relaxed text-[var(--color-muted)]" data-testid="make-body">
          {chosen.body}
        </div>
        {!forNext && <div className="text-xs text-[var(--color-muted)]">
          {archive
            ? tr("fpkg.make.archive_later", undefined, "An archive is looked at after it is unpacked, so no choice is marked here.")
            : !checked
            ? tr("fpkg.make.pick_game", undefined, "Check a game under ① and the better choice for it on your PS5 is marked.")
            : kraken && !ps4
              ? tr(
                  "fpkg.make.why_kraken",
                  undefined,
                  "Kraken Mount runs on this PS5: a package made here is mounted from a drive, not installed.",
                )
              : tr("fpkg.make.why_pkg", undefined, "A package is the ordinary choice: it installs and plays like any other game.")}
        </div>}
      </div>
    </Card>
  );
}
