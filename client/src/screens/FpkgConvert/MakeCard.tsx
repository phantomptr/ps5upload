import { Folder, Package, HardDrive } from "lucide-react";

import { Badge, Card } from "../../components";
import { ps5FakeGameUnplayableFirmware } from "../../lib/ps5Firmware";
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
 * game's files back as a plain folder. They are explained side by side, and the one of the
 * first two that suits the game on the connected console is marked: a PS5 game on firmware
 * above 11.60 installs as a package but will not play, so the image is the way; otherwise the
 * package is the ordinary choice. The folder is never marked: it is a way back, not a way in.
 * For a PS4 game the package is a PS4 fake package, which the engine writes from the game.
 */
export function MakeCard({
  make,
  onMake,
  kernel,
  contentId,
  checked,
  archive = false,
  locked = false,
  forNext = false,
  ps4 = false,
}: {
  make: MakeKind;
  onMake: (make: MakeKind) => void;
  /** The connected console's kernel string, for the firmware line. */
  kernel: string | null | undefined;
  /** The checked game's content id (or title id); null before a game is checked. */
  contentId: string | null | undefined;
  /** A game has been checked: the recommendation is about it. */
  checked: boolean;
  /** The source is an archive: it is looked at only once a run unpacks it. */
  archive?: boolean;
  locked?: boolean;
  /** A game is being made: this card sets what the next game added becomes (#669). */
  forNext?: boolean;
  /** The checked game is a PS4 game: the package is a PS4 one. */
  ps4?: boolean;
}) {
  const tr = useTr();
  const unplayableOn = checked ? ps5FakeGameUnplayableFirmware(kernel, contentId) : null;
  const recommended: "pkg" | "image" | null = !checked ? null : unplayableOn ? "image" : "pkg";
  const tiles: { id: MakeKind; icon: typeof Package; title: string; body: string }[] = [
    ps4
      ? {
          id: "pkg",
          icon: Package,
          title: tr("fpkg.make.ps4_pkg_title", undefined, "A PS4 package (.pkg)"),
          body: tr(
            "fpkg.make.ps4_pkg_body",
            undefined,
            "A PS4 fake package the PS5 installs like a game from the store, streamed from here or uploaded first. It is made with the usual fake keys and read back before it is kept.",
          ),
        }
      : {
          id: "pkg",
          icon: Package,
          title: tr("fpkg.make.pkg_title", undefined, "A package (.pkg)"),
          body: tr(
            "fpkg.make.pkg_body",
            undefined,
            "Installs on the PS5 like a game from the store, streamed from here or uploaded first. PS4 games play on any firmware; PS5 games on 11.60 and below.",
          ),
        },
    {
      id: "image",
      icon: HardDrive,
      title: tr("fpkg.make.image_title", undefined, "A game image"),
      body: tr(
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
        <div className="grid grid-cols-3 gap-1.5 sm:gap-2" role="radiogroup" data-testid="make-choice">
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
                      {t.id === "image" && unplayableOn
                        ? tr("fpkg.make.recommended_fw", { fw: unplayableOn }, "Recommended on FW {fw}")
                        : tr("fpkg.make.recommended", undefined, "Recommended")}
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
            : unplayableOn
              ? tr(
                  "fpkg.make.why_image",
                  { fw: unplayableOn },
                  "This PS5 game would install as a package but not play on FW {fw}; as an image it does.",
                )
              : tr("fpkg.make.why_pkg", undefined, "A package is the ordinary choice: it installs and plays like any other game.")}
        </div>}
      </div>
    </Card>
  );
}
