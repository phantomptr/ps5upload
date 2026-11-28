import { AlertTriangle, Info } from "lucide-react";

import { Button, Toggle } from "../../components";
import type { AmprLz4Options, AmprPacksReadiness, FfpfscLevel, ImageFormat, PlainAmprEmu, ZstdPacks } from "../../api/fpkg";
import {
  DEFAULT_VERIFY_CHUNK_KIB,
  VERIFY_CHUNK_KIB,
  parseAmprEmuBuild,
  parseZstdRuntime,
  useConvertPrefs,
  type AmprEmuBuild,
} from "../../state/convertPrefs";
import { useTr, type Translator } from "../../state/lang";

// The options of a game image (③ Options when "A game image" is what Convert makes): its
// filesystem, .ffpfsc compression, AMPR LZ4 or zstd asset packs, and how the read-back check
// reads. The folder options take the same pack choice.

/** The asset-pack choice for a game's image or folder: LZ4 (a libSceAmpr game's own ampr_emu)
 *  or zstd (ampr-emu-ng). */
export interface Lz4Choice {
  /** What the check found about the game's own ampr_emu. */
  readiness: AmprPacksReadiness;
  on: boolean;
  onToggle: (on: boolean) => void;
  /** A profile read from a .toml file (its name); null packs with the built-in one. */
  profileName: string | null;
  /** The profile came from this title's measured play, not from a file: an LZ4 default that
   *  zstd packs don't take. */
  profileMeasured?: boolean;
  onProfile: (profile: { name: string; text: string } | null) => void;
  /** Which packs: LZ4 (the game's own emulator) or zstd (ampr-emu-ng goes in). Either goes into
   *  any image, compressed into a .ffpfsc or not, and into a folder. */
  kind: "lz4" | "zstd";
  onKind: (kind: "lz4" | "zstd") => void;
  zstdLevel: number;
  onZstdLevel: (level: number) => void;
  /** A PS4 game: no asset packs at all (ampr-emu-ng is a PS5 library). */
  ps4?: boolean;
  /** The packs the game is in now, if any (the check's `packs`). */
  packed?: "lz4" | "zstd" | null;
  /** "Unpack the asset packs" is ticked: the game is written as plain files. */
  unpacking?: boolean;
}

/** Whether zstd packs are offered: for any PS5 game. One that doesn't read through AMPR gets
 *  ampr-emu-ng injected all the same, as ps5-dump-forge does it, marked as untested. */
export function zstdOffered(lz4: Lz4Choice | undefined): lz4 is Lz4Choice {
  return !!lz4 && !lz4.ps4;
}

/** Whether LZ4 packs are offered: for a game that reads through AMPR (its own ampr_emu, or the
 *  release one put in, reads them), in any image and in a folder. */
export function lz4Offered(lz4: Lz4Choice | undefined): lz4 is Lz4Choice {
  return !!lz4 && lz4.readiness.kind !== "no_ampr" && !lz4.ps4;
}

/** The zstd levels offered, as ps5-dump-forge offers them. */
const ZSTD_LEVELS = [1, 5, 19];

/** What the check found about the game's own emulator: `runtime` when a refusal comes without
 *  a kind (an older engine). */
export function readinessKind(r: AmprPacksReadiness): "ok" | "runtime" | "no_ampr" {
  return r.kind ?? (r.refusal ? "runtime" : "ok");
}

/** LZ4 packs for this game put ampr_emu 0.4.2.1 in: its own emulator is missing or can't read
 *  them. Not a choice: without it the packs would not start (#828). */
export function installRuntimeFor(lz4: Lz4Choice): boolean {
  return readinessKind(lz4.readiness) === "runtime";
}

/** Why the chosen packs can't be made for this game, or null. */
export function lz4Refused(lz4: Lz4Choice): string | null {
  if (!lz4.readiness.refusal) return null;
  // zstd packs bring their own emulator, so the game's own doesn't matter: only a PS4 game is
  // out.
  if (lz4.kind === "zstd" && zstdOffered(lz4)) return null;
  // LZ4 packs for a game whose own emulator can't read them bring ampr_emu 0.4.2.1.
  if (lz4.kind === "lz4" && installRuntimeFor(lz4)) return null;
  return lz4.readiness.refusal;
}

/** The AMPR emulator of an image or folder, by the packs chosen (#828):
 *  - `none`: nothing to choose (no AMPR, or the game keeps the emulator it has);
 *  - `choose`: no packs, and the game reads through AMPR with no emulator: add one, either kind;
 *  - `fixed`: only one emulator works, so it is that one: ampr-emu-ng with zstd packs (drakmor's
 *    ampr_emu reads LZ4 packs only), ampr_emu with LZ4 packs. `added`: `always` when the packs
 *    bring it, `switch` when it goes in only with "Add the AMPR emulator" on (a game kept in
 *    the packs it is in, written as it is). */
export type EmulatorRule =
  | { kind: "none" }
  | { kind: "choose" }
  | { kind: "fixed"; build: AmprEmuBuild; added: "always" | "switch" };

export function emulatorRule(lz4: Lz4Choice | undefined): EmulatorRule {
  if (!lz4 || lz4.ps4) return { kind: "none" };
  if (lz4Active(lz4)) {
    if (lz4.kind === "zstd") return { kind: "fixed", build: "ampr-emu-ng", added: "always" };
    return installRuntimeFor(lz4) ? { kind: "fixed", build: "ampr_emu", added: "always" } : { kind: "none" };
  }
  if (!lz4.readiness.emulator_missing) return { kind: "none" };
  // Written as it is, packs and all: the emulator has to read those packs.
  if (lz4.packed && !lz4.unpacking)
    return { kind: "fixed", build: lz4.packed === "zstd" ? "ampr-emu-ng" : "ampr_emu", added: "switch" };
  return { kind: "choose" };
}

/** What an image build is told when packs are on, or nothing. Packs go into any image, plain or
 *  compressed into a .ffpfsc (as PS5 Dump Forge offers them). */
export function packOptions(lz4: Lz4Choice | undefined, profileToml?: string): AmprLz4Options | undefined {
  if (!lz4 || !lz4Active(lz4)) return undefined;
  return lz4.kind === "zstd"
    ? { zstdLevel: lz4.zstdLevel, zstdRuntime: useConvertPrefs.getState().zstdRuntime, profileToml }
    : { profileToml, installRuntime: installRuntimeFor(lz4) };
}

/** Whether a pack format is on: for any image (any format, compressed or not) and a folder. */
export function lz4Active(lz4: Lz4Choice | undefined): boolean {
  return !!lz4?.on && !lz4Refused(lz4);
}

/** Whether a folder build packs (LZ4 or zstd). */
export function folderPacks(lz4: Lz4Choice | undefined): boolean {
  return lz4Active(lz4);
}

/** What a folder build is told when zstd packs are on, or nothing. */
export function folderPackOptions(
  lz4: Lz4Choice | undefined,
  profileToml?: string,
): ZstdPacks | undefined {
  return lz4 && folderPacks(lz4) && lz4.kind === "zstd"
    ? { level: lz4.zstdLevel, runtime: useConvertPrefs.getState().zstdRuntime, profileToml }
    : undefined;
}

/** What a folder build is told when LZ4 packs are on, or nothing. */
export function folderLz4Options(
  lz4: Lz4Choice | undefined,
  profileToml?: string,
): AmprLz4Options | undefined {
  return lz4 && folderPacks(lz4) && lz4.kind === "lz4"
    ? { profileToml, installRuntime: installRuntimeFor(lz4) }
    : undefined;
}

/** A pack profile (.toml) picked from this device, or the built-in rules: the one picker LZ4
 *  and zstd packs share. */
function ProfilePicker({ lz4, zstd }: { lz4: Lz4Choice; zstd?: boolean }) {
  const tr = useTr();
  // zstd packs take a profile picked by hand only; a measured one is LZ4's.
  const named = zstd && lz4.profileMeasured ? null : lz4.profileName;
  return (
    <div className={`flex flex-wrap items-center gap-2 text-xs${zstd ? " ms-6" : ""}`} data-testid="pack-profile">
      {named ? (
        <>
          <span className="text-[var(--color-muted)]">
            {tr("fpkg.image.lz4_profile_named", { name: named }, "Profile: {name}")}
          </span>
          <Button variant="ghost" onClick={() => lz4.onProfile(null)}>
            {tr("fpkg.image.lz4_profile_clear", undefined, "Use the built-in profile")}
          </Button>
        </>
      ) : (
        <label className="cursor-pointer text-[var(--color-accent)] underline">
          {tr("fpkg.image.lz4_profile", undefined, "Use a profile (.toml)…")}
          <input
            type="file"
            accept=".toml"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0];
              e.target.value = "";
              if (file) void file.text().then((text) => lz4.onProfile({ name: file.name, text }));
            }}
          />
        </label>
      )}
      {zstd && named && (
        <span className="basis-full leading-relaxed text-[var(--color-muted)]" data-testid="zstd-profile-note">
          {tr(
            "fpkg.image.zstd_profile_note",
            undefined,
            "For zstd packs the profile only decides which files stay loose: a file is packed unless its rule (or default_action, which is loose unless the profile sets it) says loose. Block sizes, hot, layout and LZ4 settings are ignored. System files always stay loose.",
          )}
        </span>
      )}
    </div>
  );
}

/** The tested ampr-emu-ng release, as the engine pins it (zstd_packs.rs). */
const NG_TESTED = "0.4.1";

/** Which ampr-emu-ng goes in with zstd packs: the newest release or the tested one. */
function ZstdRuntimeChoice() {
  const tr = useTr();
  const runtime = useConvertPrefs((s) => s.zstdRuntime);
  const set = useConvertPrefs((s) => s.setZstdRuntime);
  return (
    <div className="ms-6 space-y-0.5 text-xs">
      <label className="flex items-center gap-2">
        <span>{tr("fpkg.image.zstd_runtime", undefined, "Pack runtime")}</span>
        <select
          value={runtime}
          onChange={(e) => set(parseZstdRuntime(e.target.value))}
          className="select w-auto!"
          data-testid="zstd-runtime"
        >
          <option value="latest">
            {tr("fpkg.image.zstd_runtime_latest", undefined, "Newest release (default)")}
          </option>
          <option value="pinned">
            {tr("fpkg.image.zstd_runtime_pinned", { version: NG_TESTED }, "Tested {version}")}
          </option>
        </select>
      </label>
      <div className="leading-relaxed text-[var(--color-muted)]">
        {tr(
          "fpkg.image.zstd_runtime_body",
          { version: NG_TESTED },
          "Newest uses the latest ampr-emu-ng release on GitHub only when it matches the SHA-256 GitHub publishes for it; otherwise, or offline, the tested {version} goes in. Tested always uses {version}.",
        )}
      </div>
    </div>
  );
}

/** zstd packs, for any plain image of a game that reads through AMPR, or for a plain folder
 *  (`folder`), the only packs a folder gets. */
export function ZstdOption({ lz4, folder }: { lz4: Lz4Choice; folder?: boolean }) {
  const tr = useTr();
  const on = lz4.on && lz4.kind === "zstd";
  return (
    <div className="space-y-1" data-testid="zstd-option">
      <label className="flex cursor-pointer items-start gap-2.5 text-xs">
        <input
          type="checkbox"
          checked={on}
          onChange={(e) => {
            lz4.onKind("zstd");
            lz4.onToggle(e.target.checked);
          }}
          className="checkbox mt-px h-4 w-4 shrink-0"
        />
        <span className="min-w-0">
          <span className="block font-medium text-[var(--color-text)]">
            {tr("fpkg.image.zstd", undefined, "zstd asset packs (smallest, for ShadowMount+)")}
          </span>
          <span className="mt-0.5 block leading-relaxed text-[var(--color-muted)]">
            {folder
              ? tr(
                  "fpkg.folder.zstd_body_v2",
                  undefined,
                  "Writes the game as a plain folder with all but its system files in zstd packs, and quer3q's ampr-emu-ng as fakelib/libSceAmpr.sprx. A game folder can be the source too. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read them.",
                )
              : tr(
                  "fpkg.image.zstd_body",
                  undefined,
                  "Packs all but the game's system files into zstd packs, written straight into the image with no temporary copy. The image gets quer3q's ampr-emu-ng as its emulator, checked against a published digest before it is used. A game already in LZ4 packs is switched over. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read them.",
                )}
          </span>
        </span>
      </label>
      {lz4.readiness.kind === "no_ampr" && (
        <div className="ms-6 flex items-start gap-1.5 text-xs text-[var(--color-warn)]" data-testid="zstd-no-ampr">
          <AlertTriangle size={13} className="mt-px shrink-0" aria-hidden />
          <span>
            {tr(
              "fpkg.image.zstd_no_ampr",
              undefined,
              "This game doesn't read its files through AMPR. zstd packs for such games are new and not tested on every game; keep the original until you have started the packed one.",
            )}
          </span>
        </div>
      )}
      {on && (
        <div className="ms-6 text-xs text-[var(--color-muted)]" data-testid="zstd-runtime-included">
          {tr(
            "fpkg.image.zstd_runtime_included",
            undefined,
            "The zstd runtime (ampr-emu-ng) goes in with the packs, so no other emulator is needed.",
          )}
        </div>
      )}
      {on && <PacksWarning />}
      {on && <ZstdRuntimeChoice />}
      {on && <ProfilePicker lz4={lz4} zstd />}
      {on && (
        <label className="ms-6 flex items-center gap-2 text-xs">
          <span>{tr("fpkg.image.zstd_level", undefined, "Compression level")}</span>
          <select
            value={lz4.zstdLevel}
            onChange={(e) => lz4.onZstdLevel(Number(e.target.value))}
            className="select w-auto!"
            data-testid="zstd-level"
          >
            {ZSTD_LEVELS.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </label>
      )}
    </div>
  );
}

/** LZ4 packs, for any image of a game that reads through AMPR, or for a folder (`folder`). */
export function Lz4Option({ lz4, folder }: { lz4: Lz4Choice; folder?: boolean }) {
  const tr = useTr();
  const refusal = lz4Refused({ ...lz4, kind: "lz4" });
  const chosen = lz4.on && lz4.kind === "lz4";
  // A game whose own emulator can't read packs gets ampr_emu 0.4.2.1 with them: said under LZ4
  // packs picked, never a separate box to untick (#828).
  const runtime = installRuntimeFor(lz4);
  return (
    <div className="space-y-1" data-testid="lz4-option">
      <label className="flex cursor-pointer items-start gap-2.5 text-xs">
        <input
          type="checkbox"
          checked={chosen && !refusal}
          disabled={!!refusal}
          onChange={(e) => {
            lz4.onKind("lz4");
            lz4.onToggle(e.target.checked);
          }}
          className="checkbox mt-px h-4 w-4 shrink-0"
        />
        <span className="min-w-0">
          <span className="block font-medium text-[var(--color-text)]">
            {tr("fpkg.image.lz4", undefined, "AMPR LZ4 asset packs (smaller, for ShadowMount+)")}
          </span>
          <span className="mt-0.5 block leading-relaxed text-[var(--color-muted)]">
            {folder
              ? tr(
                  "fpkg.folder.lz4_body",
                  undefined,
                  "Writes the game as a plain folder with its data in LZ4 asset packs that the game's own ampr_emu (fakelib/libSceAmpr.sprx) reads back, so the folder is smaller. Executables, system files and already-compressed media stay as they are. A game folder can be the source too. The packs are made beside the folder first, so it needs that much room again while it is made.",
                )
              : tr(
                  "fpkg.image.lz4_body_v2",
                  undefined,
                  "Packs the game's data into LZ4 asset packs that the game's own ampr_emu (fakelib/libSceAmpr.sprx) reads back, so the image is smaller. Executables, system files and already-compressed media stay as they are. Works with every image format, and inside a .ffpfsc. The packs are made on this computer first, so it needs that much free space while the image is made, even when the image goes to the PS5 or a server.",
                )}
          </span>
        </span>
      </label>
      {refusal ? (
        <div className="text-xs text-[var(--color-warn)]">
          {tr("fpkg.image.lz4_unavailable", { reason: refusal }, "Not available for this game: {reason}")}
        </div>
      ) : runtime ? (
        // The emulator that reads them goes in with them: said once they are picked.
        chosen && (
          <div className="ms-6 text-xs leading-relaxed text-[var(--color-muted)]" data-testid="lz4-install-runtime">
            {folder
              ? tr(
                  "fpkg.folder.lz4_install_runtime_body",
                  { reason: lz4.readiness.refusal ?? "" },
                  "{reason}. The folder gets drakmor's ampr_emu 0.4.2.1 as fakelib/libSceAmpr.sprx (fetched once from its GitHub release and checked against a known digest); the game's own copy stays beside it as libSceAmpr.sprx.orig.",
                )
              : tr(
                  "fpkg.image.lz4_install_runtime_body",
                  { reason: lz4.readiness.refusal ?? "" },
                  "{reason}. The image gets drakmor's ampr_emu 0.4.2.1 as fakelib/libSceAmpr.sprx (fetched once from its GitHub release and checked against a known digest); the game's own copy stays beside it as libSceAmpr.sprx.orig.",
                )}
          </div>
        )
      ) : (
        lz4.readiness.runtime_version && (
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "fpkg.image.lz4_runtime",
              { version: lz4.readiness.runtime_version },
              "The game's ampr_emu: {version}",
            )}
          </div>
        )
      )}
      {lz4.on && lz4.kind === "lz4" && !refusal && <PacksWarning />}
      {lz4.on && lz4.kind === "lz4" && !refusal && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="lz4-placement-note">
          {tr(
            "fpkg.image.lz4_placement_note",
            undefined,
            "Keep a packed image on the console's internal storage: one has been reported to hang on a black screen from USB. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read packs.",
          )}
        </div>
      )}
      {lz4.on && lz4.kind === "lz4" && !refusal && <ProfilePicker lz4={lz4} />}
    </div>
  );
}

/** For a game that is in asset packs, when no pack option is on: write it as plain files
 *  again. Without it the game goes through as it is, packs and all. */
export interface UnpackChoice {
  /** The packs the game is in. */
  kind: "lz4" | "zstd";
  on: boolean;
  onToggle: (on: boolean) => void;
  /** A package's (#933): leave the AMPR emulator out once the files are plain. Absent for an
   *  image or folder, which ShadowMount+ mounts, where some AMPR games need the emulator. */
  leaveOutEmulator?: { on: boolean; onToggle: (on: boolean) => void };
}

/** `compact`: the two choices by name only, for a place that has no room for what each does
 *  (a waiting game's options in the queue; ③ explains them). */
export function UnpackOption({ unpack, compact }: { unpack: UnpackChoice; compact?: boolean }) {
  const tr = useTr();
  const kind = unpack.kind === "zstd" ? "zstd" : "LZ4";
  const leaveOut = unpack.leaveOutEmulator;
  return (
    <div className="flex flex-col gap-2">
      <label className="flex cursor-pointer items-start gap-2.5 text-xs" data-testid="unpack-option">
        <input
          type="checkbox"
          checked={unpack.on}
          onChange={(e) => unpack.onToggle(e.target.checked)}
          className="checkbox mt-px h-4 w-4 shrink-0"
        />
        <span className="min-w-0">
          <span className="block font-medium text-[var(--color-text)]">
            {tr("fpkg.unpack_packs", undefined, "Unpack the asset packs (plain files again)")}
          </span>
          <span className={compact ? "hidden" : "mt-0.5 block leading-relaxed text-[var(--color-muted)]"}>
            {leaveOut
              ? tr(
                  "fpkg.unpack_packs_pkg_body",
                  { kind },
                  "This game is in {kind} asset packs. Ticked, the package holds every file as a plain file and leaves the packs out, so it is about as large as the game was before packing. An emulator that only suits packs is replaced by the one chosen in the options. Unticked, the packs go in as they are, and only the emulator can read them.",
                )
              : tr(
                  "fpkg.unpack_packs_body",
                  { kind },
                  "This game is in {kind} asset packs. Ticked, every packed file is written as a plain file again and the packs are left out, so the game is as large as it was before packing and no longer needs ShadowMount+ to read packs. It gets a fresh ampr_emu.index, and a game that reads its files through AMPR whose emulator only suits packs gets the emulator chosen in the options. Unticked, the game is written as it is, packs and all.",
                )}
          </span>
        </span>
      </label>
      {leaveOut && unpack.on && (
        <label className="ms-6 flex cursor-pointer items-start gap-2.5 text-xs" data-testid="leave-out-ampr-emu">
          <input
            type="checkbox"
            checked={leaveOut.on}
            onChange={(e) => leaveOut.onToggle(e.target.checked)}
            className="checkbox mt-px h-4 w-4 shrink-0"
          />
          <span className="min-w-0">
            <span className="block font-medium text-[var(--color-text)]">
              {tr("fpkg.leave_out_ampr_emu", undefined, "Leave out the AMPR emulator")}
            </span>
            <span className={compact ? "hidden" : "mt-0.5 block leading-relaxed text-[var(--color-muted)]"}>
              {tr(
                "fpkg.leave_out_ampr_emu_body",
                undefined,
                "The package then has no fakelib/libSceAmpr.sprx, and the game reads its files through the PS5's own system library. This has been tried with one game, on firmware 5.10. If the game doesn't start, make the package again with this off.",
              )}
            </span>
          </span>
        </label>
      )}
    </div>
  );
}

/** For a package of a game in no asset packs that carries an AMPR emulator the check says can go:
 *  leave it out. Off unless turned on. */
export interface PlainEmulatorChoice {
  on: boolean;
  onToggle: (on: boolean) => void;
  /** What it is ("ampr_emu 0.4.2.1"), for the note; absent in the queue, which has no room. */
  emulator?: string;
  /** The game's eboot.bin imports libSceAmpr (the PS5's own library then serves it). */
  importsAmpr?: boolean;
}

function PlainEmulatorOption({ choice, compact }: { choice: PlainEmulatorChoice; compact?: boolean }) {
  const tr = useTr();
  const emulator = choice.emulator ?? "";
  return (
    <label className="flex cursor-pointer items-start gap-2.5 text-xs" data-testid="leave-out-plain-ampr-emu">
      <input
        type="checkbox"
        checked={choice.on}
        onChange={(e) => choice.onToggle(e.target.checked)}
        className="checkbox mt-px h-4 w-4 shrink-0"
      />
      <span className="min-w-0">
        <span className="block font-medium text-[var(--color-text)]">
          {tr("fpkg.leave_out_ampr_emu", undefined, "Leave out the AMPR emulator")}
        </span>
        <span className={compact ? "hidden" : "mt-0.5 block leading-relaxed text-[var(--color-muted)]"}>
          {choice.importsAmpr === false
            ? tr(
                "cw_convert_plain_leave_out_note_unused",
                { emulator },
                "This game has no asset packs but carries an AMPR emulator ({emulator}) that its eboot.bin doesn't use. On, the package leaves out its libSceAmpr.sprx and the emulator's logs, and keeps the other fakelib files.",
              )
            : tr(
                "cw_convert_plain_leave_out_note",
                { emulator },
                "This game has no asset packs but carries an AMPR emulator ({emulator}). On, the package leaves out its libSceAmpr.sprx and the emulator's logs, and keeps the other fakelib files. The game then reads its files through the PS5's own system library. This has been tried with one game, on firmware 5.10. If the game doesn't start, make the package again with this off.",
              )}
        </span>
      </span>
    </label>
  );
}

/** Why a plain game's AMPR emulator stays in its package (the check's `refusal`), in a line;
 *  null when it can go or the game carries none. */
export function plainEmulatorKeptText(
  tr: Translator,
  found: PlainAmprEmu | null | undefined,
): string | null {
  if (!found?.refusal) return null;
  switch (found.refusal) {
    case "not_an_emulator":
      return tr(
        "cw_convert_plain_emu_kept_not_emulator",
        undefined,
        "The AMPR emulator stays: this game's libSceAmpr.sprx isn't an AMPR emulator. It may be a newer system library that a backport needs.",
      );
    case "pack_files":
      return tr(
        "cw_convert_plain_emu_kept_pack_files",
        undefined,
        "The AMPR emulator stays: this game has asset pack files, and only the emulator reads them.",
      );
    case "index_lists_missing":
      return tr(
        "cw_convert_plain_emu_kept_index_missing",
        undefined,
        "The AMPR emulator stays: this game's ampr_emu.index lists files the game doesn't have. They may be in asset packs that only the emulator reads.",
      );
    default:
      return tr("cw_convert_plain_emu_kept_other", undefined, "The AMPR emulator stays: it can't be left out of this game.");
  }
}

/** A package's asset-pack choices (#933), in one named place at the top of the options, so
 *  someone looking for "AMPR" or "asset packs" finds them or finds why they aren't there:
 *  - a game in packs (`unpack`): "Unpack the asset packs", and under it "Leave out the AMPR
 *    emulator";
 *  - a game in no packs that carries its AMPR emulator ("none-emulator"): "Leave out the AMPR
 *    emulator" when the check says it can go, else why it stays;
 *  - a game being checked ("checking"): that the options come when the check is done;
 *  - a game that was checked and is in no packs (`state` "none"): one line saying so, and when
 *    it carries an AMPR emulator, "Leave out the AMPR emulator" (`plainEmulator`) or why it
 *    stays (`plainKept`);
 *  - a game not checked yet ("pending"): one line saying that checking it shows them;
 *  - a waiting game that was never checked ("unchecked", the queue): what happens to it. */
/** What a package's asset-pack line says when there is nothing to choose: see {@link PackagePacks}. */
export type PackagePacksState = "pending" | "checking" | "none" | "none-emulator" | "unchecked";

export function PackagePacks({
  unpack,
  plainEmulator,
  plainKept,
  state,
  compact,
}: {
  unpack?: UnpackChoice;
  state: PackagePacksState;
  plainEmulator?: PlainEmulatorChoice;
  plainKept?: string | null;
  compact?: boolean;
}) {
  const tr = useTr();
  const title = tr("fpkg.pkg_packs.title", undefined, "Asset packs and AMPR");
  if (unpack || plainEmulator) {
    return (
      <div
        role="group"
        aria-label={title}
        className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-2.5"
        data-testid="pkg-packs"
      >
        <div className="text-xs font-medium text-[var(--color-muted)]">{title}</div>
        {unpack ? (
          <UnpackOption unpack={unpack} compact={compact} />
        ) : plainEmulator ? (
          <PlainEmulatorOption choice={plainEmulator} compact={compact} />
        ) : null}
      </div>
    );
  }
  if ((state === "none" || state === "none-emulator") && plainKept) {
    return (
      <div className="text-xs text-[var(--color-muted)]" data-testid="pkg-packs-note" data-state="emulator-kept">
        <span className="font-medium">{title}</span>
        <p className="leading-relaxed">{plainKept}</p>
      </div>
    );
  }
  const text =
    state === "none"
      ? tr("fpkg.pkg_packs.none", undefined, "This game has no asset packs, so there is nothing to unpack.")
      : state === "none-emulator"
      ? tr(
          "fpkg.pkg_packs.none_emulator_v2",
          undefined,
          "This game has no asset packs, so there is nothing to unpack. Its AMPR emulator (fakelib/libSceAmpr.sprx) goes into the package as it is.",
        )
      : state === "checking"
      ? tr(
          "fpkg.pkg_packs.checking",
          undefined,
          "Checking the game under ①. If it is in asset packs, the options to unpack them and to leave out the AMPR emulator appear here when the check is done.",
        )
      : state === "unchecked"
        ? tr(
            "fpkg.pkg_packs.unchecked",
            undefined,
            "This game wasn't checked under ①. If it is in asset packs, the package gets them unpacked. To keep the packs or leave out the AMPR emulator, check the game under ① and add it from there.",
          )
        : tr(
            "fpkg.pkg_packs.pending",
            undefined,
            "Check a game under ① first. If it is in asset packs, the options to unpack them and to leave out the AMPR emulator appear here.",
          );
  return (
    <div className="text-xs text-[var(--color-muted)]" data-testid="pkg-packs-note" data-state={state}>
      <span className="font-medium">{title}</span>
      <p className="leading-relaxed">{text}</p>
    </div>
  );
}

/** Under a pack option that is on: a packed game needs ShadowMount+ to mount it. */
function PacksWarning() {
  const tr = useTr();
  return (
    <div className="ms-6 flex items-start gap-1.5 text-xs text-[var(--color-warn)]" data-testid="packs-warning">
      <AlertTriangle size={13} className="mt-px shrink-0" aria-hidden />
      <span>
        {tr(
          "fpkg.image.packs_shadowmount_only",
          undefined,
          "A packed game only plays when ShadowMount+ mounts it. PS5Upload's own mount can't read the packs. Plain images work either way.",
        )}
      </span>
    </div>
  );
}

/** One muted line where the asset packs would be, saying why they aren't: no game checked yet
 *  (`lz4` absent), or a PS4 game. Nothing when packs are offered. */
export function PacksNote({ lz4 }: { lz4: Lz4Choice | undefined }) {
  const tr = useTr();
  // A folder takes the same packs as an image.
  const text = !lz4
    ? tr(
        "fpkg.image.packs_pending_v2",
        undefined,
        "Asset packs (LZ4, zstd) appear here once a PS5 game is checked.",
      )
    : lz4.ps4
      ? tr(
          "fpkg.image.packs_no_ampr",
          undefined,
          "Asset packs: not offered, this game doesn't read its files through AMPR.",
        )
      : null;
  if (!text) return null;
  return (
    <p className="text-xs text-[var(--color-muted)]" data-testid="packs-note">
      {text}
    </p>
  );
}

/** Format names, the same in every language. */
const FORMAT_NAMES = {
  ffpkg: ".ffpkg (UFS2)",
  exfat: ".exfat",
  ffpfs: ".ffpfs (PFS)",
  ffpfsc: ".ffpfsc",
};

/** "Which image should I make?": a few plain lines on each kind of image and on asset packs.
 *  A native disclosure, so it opens by keyboard and touch alike. */
function WhichImageHelp() {
  const tr = useTr();
  const rows: [string, string][] = [
    [FORMAT_NAMES.ffpkg, tr("fpkg.image.which_ffpkg", undefined, "Recommended. Works best.")],
    [FORMAT_NAMES.exfat, tr("fpkg.image.which_exfat", undefined, "The common format other tools use.")],
    [FORMAT_NAMES.ffpfs, tr("fpkg.image.which_ffpfs", undefined, "Sony's own format. Experimental.")],
    [
      FORMAT_NAMES.ffpfsc,
      tr(
        "fpkg.image.which_ffpfsc",
        undefined,
        "Compression: shrinks the whole image, and the PS5 unpacks it while reading. Fast, Balanced or Smallest trade making time for size.",
      ),
    ],
    [
      tr("fpkg.image.which_packs_label_v2", undefined, "Asset packs (LZ4: AMPR games only)"),
      tr(
        "fpkg.image.which_packs",
        undefined,
        "Compress the game's data and add a small replacement for Sony's AMPR library that unpacks it. Only ShadowMount+ loads that replacement, so a packed game must be mounted by ShadowMount+.",
      ),
    ],
  ];
  return (
    <details className="text-xs" data-testid="which-image">
      <summary className="inline-flex cursor-pointer list-none items-center gap-1.5 rounded text-[var(--color-accent)] [&::-webkit-details-marker]:hidden">
        <Info size={13} className="shrink-0" aria-hidden />
        {tr("fpkg.image.which_title", undefined, "Which image should I make?")}
      </summary>
      <dl className="mt-1.5 flex flex-col gap-1 leading-relaxed text-[var(--color-muted)]">
        {rows.map(([name, body]) => (
          <div key={name}>
            <dt className="inline font-medium text-[var(--color-text)]">{name}</dt>{" "}
            <dd className="inline">{body}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}

/** Some libSceAmpr games with no emulator fail to start from an image (Alembic's two on a console),
 *  others run (LEGO Horizon Adventures reaches its menu with or without one, on the Pro), and
 *  ShadowMount+ doesn't add one. On by default. Only for an image without new packs: the engine
 *  leaves it out for packs, which bring their own emulator (#828). `fixed`: the game stays in
 *  the packs it is in, so only the emulator that reads them is offered. */
function AmprEmuOption({ fixed, packed }: { fixed?: AmprEmuBuild; packed?: "lz4" | "zstd" | null }) {
  const tr = useTr();
  const on = useConvertPrefs((s) => s.addAmprEmu);
  const set = useConvertPrefs((s) => s.setAddAmprEmu);
  const build = useConvertPrefs((s) => s.amprEmuBuild);
  const setBuild = useConvertPrefs((s) => s.setAmprEmuBuild);
  return (
    <div className="space-y-1" data-testid="ampr-emu-option">
      <Toggle
        checked={on}
        onChange={set}
        label={tr("fpkg.image.ampr_emu", undefined, "Add the AMPR emulator")}
        hint={tr(
          "fpkg.image.ampr_emu_body_v2",
          undefined,
          "For an image without asset packs: this game reads its files through AMPR and has no emulator. Some such games won't start from an image without one, and others run fine. Adds drakmor's ampr_emu 0.4.2.1 as fakelib/libSceAmpr.sprx, fetched once from its GitHub release and checked against a known digest. Asset packs bring their own emulator instead.",
        )}
      />
      {on &&
        (fixed ? (
          <div className="text-xs text-[var(--color-muted)]" data-testid="ampr-emu-fixed">
            {tr(
              "fpkg.ampr.emu_fixed_packs",
              { kind: packed === "zstd" ? "zstd" : "LZ4", emulator: fixed === "ampr-emu-ng" ? AMPR_EMU_NG : AMPR_EMU },
              "This game stays in its {kind} packs, so it gets {emulator}, the emulator that reads them.",
            )}
          </div>
        ) : (
          <AmprEmuBuildChoice build={build} onBuild={setBuild} />
        ))}
    </div>
  );
}

/** Product names, the same in every language. */
const AMPR_EMU = "ampr_emu 0.4.2.1";
const AMPR_EMU_NG = "ampr-emu-ng";

/** Everything AMPR for one game, in one place and in order (#828): the asset packs first, then
 *  only the emulator that goes with them. A game that doesn't read through AMPR gets one line
 *  saying so, and zstd packs only (they bring their own runtime); a PS4 game, or no game
 *  checked yet (`packsNote`), one line. The emulator switch is an image's: a folder build adds
 *  one only when it writes packs out as plain files. */
export function AmprSection({ lz4, folder, packsNote }: { lz4?: Lz4Choice; folder?: boolean; packsNote?: boolean }) {
  const tr = useTr();
  if (!lz4 || lz4.ps4) return lz4 || packsNote ? <PacksNote lz4={lz4} /> : null;
  const rule = emulatorRule(lz4);
  return (
    <div
      role="group"
      aria-label={tr("cq_packs", undefined, "Asset packs")}
      className="space-y-2 rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-2.5"
      data-testid="ampr-section"
    >
      <div className="text-xs font-medium text-[var(--color-muted)]">{tr("cq_packs", undefined, "Asset packs")}</div>
      {lz4.readiness.kind === "no_ampr" && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="ampr-not-used">
          {tr("fpkg.ampr.not_used", undefined, "This game doesn't use AMPR, so it needs no AMPR emulator.")}
        </p>
      )}
      {lz4Offered(lz4) && <Lz4Option lz4={lz4} folder={folder} />}
      {zstdOffered(lz4) && <ZstdOption lz4={lz4} folder={folder} />}
      {!folder && rule.kind === "choose" && <AmprEmuOption />}
      {!folder && rule.kind === "fixed" && rule.added === "switch" && (
        <AmprEmuOption fixed={rule.build} packed={lz4.packed} />
      )}
    </div>
  );
}

/** Which emulator a game gets: drakmor's, or quer3q's, which also reads zstd packs. */
export function AmprEmuBuildChoice({
  build,
  onBuild,
}: {
  build: AmprEmuBuild;
  onBuild: (build: AmprEmuBuild) => void;
}) {
  const tr = useTr();
  return (
    <label className="flex items-center gap-2 text-xs">
      <span>{tr("fpkg.image.ampr_emu_build", undefined, "Emulator")}</span>
      <select
        value={build}
        onChange={(e) => onBuild(parseAmprEmuBuild(e.target.value))}
        className="select w-auto!"
        data-testid="ampr-emu-build"
      >
        <option value="ampr_emu">{AMPR_EMU}</option>
        <option value="ampr-emu-ng">{AMPR_EMU_NG}</option>
      </select>
    </label>
  );
}

/** Which image to make: the filesystem (UFS2 .ffpkg, what ShadowMount+ recommends, or exFAT),
 *  and whether to compress it into a .ffpfsc. */
export function ImageChoice({
  format,
  compress,
  onChoice,
  lz4,
  level,
  packsNote,
}: {
  format: ImageFormat;
  compress: boolean;
  onChoice: (format: ImageFormat, compress: boolean) => void;
  lz4?: Lz4Choice;
  /** Say why no pack option shows before a game is checked (Convert's ③, not a queued game). */
  packsNote?: boolean;
  /** One game's own compression level (a game in the queue); else the remembered one. */
  level?: { value: FfpfscLevel; onChange: (level: FfpfscLevel) => void };
}) {
  const tr = useTr();
  const setFormat = (f: ImageFormat) => onChoice(f, compress);
  const setCompress = (c: boolean) => onChoice(format, c);
  const options: { id: ImageFormat; label: string; body: string }[] = [
    {
      id: "ffpkg",
      label: tr("fpkg.image.ffpkg", undefined, ".ffpkg (UFS2), recommended"),
      body: tr(
        "fpkg.image.ffpkg_body",
        undefined,
        "What ShadowMount+ recommends for most games.",
      ),
    },
    {
      id: "exfat",
      label: tr("fpkg.image.exfat_choice", undefined, ".exfat"),
      body: tr(
        "fpkg.image.exfat_body",
        undefined,
        "For games that only work like content on an external drive.",
      ),
    },
    {
      id: "ffpfs",
      label: tr("fpkg.image.ffpfs", undefined, ".ffpfs (PFS), experimental"),
      body: tr(
        "fpkg.image.ffpfs_body",
        undefined,
        "Experimental in ShadowMount+ 1.7. File names must be plain ASCII; the image's own name is kept to 63 characters.",
      ),
    },
  ];
  return (
    <div className="mt-1 space-y-2" data-testid="image-choice">
      <WhichImageHelp />
      <div className="grid gap-2 2xl:grid-cols-3">
        {options.map((o) => (
          <button
            key={o.id}
            type="button"
            aria-pressed={format === o.id}
            onClick={() => setFormat(o.id)}
            className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] px-3.5 py-3 text-start text-xs ${
              format === o.id
                ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
            }`}
          >
            <div className="font-semibold text-[var(--color-text)]">{o.label}</div>
            <div className="mt-0.5 text-[var(--color-muted)]">{o.body}</div>
          </button>
        ))}
      </div>
      <label className="flex cursor-pointer items-start gap-2.5 text-xs">
        <input
          type="checkbox"
          checked={compress}
          onChange={(e) => setCompress(e.target.checked)}
          className="checkbox mt-px h-4 w-4 shrink-0"
        />
        <span className="min-w-0">
          <span className="block font-medium text-[var(--color-text)]">
            {tr("fpkg.image.compress", undefined, "Compress it into a .ffpfsc")}
          </span>
          <span className="mt-0.5 block leading-relaxed text-[var(--color-muted)]">
            {tr(
              "fpkg.image.compress_body_v2",
              undefined,
              "Usually 40–60% smaller, slower to make, and always mounted read-only. It is compressed as it is written, so only the compressed file is ever on disk.",
            )}
          </span>
        </span>
      </label>
      {compress && <FfpfscLevelOption own={level} />}
      <AmprSection lz4={lz4} packsNote={packsNote} />
      <VerifyChunkOption />
      <p className="text-xs leading-relaxed text-[var(--color-muted)]">
        {tr(
          "fpkg.image.resume_note",
          undefined,
          "If the app or the engine stops while an image is being made, making it again goes on from where it stopped and then reads every byte back. An image with LZ4 or zstd asset packs starts over.",
        )}
      </p>
    </div>
  );
}

/** How hard a .ffpfsc is compressed: Fast, Balanced (the default) or Smallest. Remembered like
 *  the other Convert options. */
export function FfpfscLevelOption({
  own,
}: {
  own?: { value: FfpfscLevel; onChange: (level: FfpfscLevel) => void };
} = {}) {
  const tr = useTr();
  const remembered = useConvertPrefs((s) => s.imageLevel);
  const remember = useConvertPrefs((s) => s.setImageLevel);
  const level = own?.value ?? remembered;
  const setLevel = own?.onChange ?? remember;
  const levels: { id: FfpfscLevel; label: string; body: string }[] = [
    {
      id: "fast",
      label: tr("fpkg.compressionFast", undefined, "Fast"),
      body: tr("fpkg.image.level_fast_body", undefined, "Quickest to make, a little larger."),
    },
    {
      id: "balanced",
      label: tr("fpkg.compressionBalanced", undefined, "Balanced"),
      body: tr("fpkg.image.level_balanced_body", undefined, "Almost as small, and several times faster to make."),
    },
    {
      id: "smallest",
      label: tr("fpkg.compressionSmallest", undefined, "Smallest"),
      body: tr(
        "fpkg.image.level_smallest_body",
        undefined,
        "Smallest file: a second, stronger encoder runs on every block, so it takes several times as long.",
      ),
    },
  ];
  return (
    <div
      role="radiogroup"
      aria-label={tr("fpkg.compression", undefined, "Compression")}
      className="ms-6 grid gap-2 2xl:grid-cols-3"
      data-testid="ffpfsc-level"
    >
      {levels.map((l) => (
        <button
          key={l.id}
          type="button"
          role="radio"
          aria-checked={level === l.id ? "true" : "false"}
          onClick={() => setLevel(l.id)}
          className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] px-3 py-2 text-start text-xs ${
            level === l.id
              ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
              : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
          }`}
        >
          <span className="flex flex-wrap items-center gap-x-2 gap-y-1 font-semibold text-[var(--color-text)]">
            {l.label}
            {l.id === "balanced" && (
              <span className="rounded-full bg-[var(--color-accent-soft)] px-2 py-0.5 text-[10px] font-medium text-[var(--color-accent)]">
                {tr("fpkg.recommended", undefined, "Recommended")}
              </span>
            )}
          </span>
          <span className="mt-0.5 block text-[var(--color-muted)]">{l.body}</span>
        </button>
      ))}
    </div>
  );
}

/** Fast (off, the default) or full verification of what Convert wrote: a package, an image or
 *  a compressed image. Fast always checks structure and sizes, whole small files and a random
 *  sample of the rest; full re-reads every byte. */
export function FullVerifyOption({
  disabled,
  own,
}: {
  disabled?: boolean;
  /** One game's own choice (a game in the queue); else the remembered one. */
  own?: { value: boolean; onChange: (full: boolean) => void };
}) {
  const tr = useTr();
  const remembered = useConvertPrefs((s) => s.fullVerify);
  const remember = useConvertPrefs((s) => s.setFullVerify);
  const full = own?.value ?? remembered;
  const set = own?.onChange ?? remember;
  return (
    <Toggle
      checked={full}
      onChange={set}
      disabled={disabled}
      label={tr("fpkg.image.full_verify", undefined, "Full verification")}
      hint={tr(
        "fpkg.image.full_verify_body",
        undefined,
        "Re-reads every byte of the output to check it. Takes longer",
      )}
    />
  );
}

/** How much the read-back check reads per call. Measured on a 10 GB image: 1–32 MiB are
 *  within 15% of each other once the reader fetches whole runs of clusters, and 8 MiB was
 *  fastest; the choice is here for anyone whose disk says otherwise. */
function VerifyChunkOption() {
  const tr = useTr();
  const kib = useConvertPrefs((s) => s.verifyChunkKib);
  const set = useConvertPrefs((s) => s.setVerifyChunkKib);
  return (
    <label className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs" data-testid="verify-chunk">
      <span className="min-w-0">
        <span className="block font-medium text-[var(--color-text)]">
          {tr("fpkg.image.verify_chunk", undefined, "Read-back check, per read")}
        </span>
        <span className="mt-0.5 block leading-relaxed text-[var(--color-muted)]">
          {tr(
            "fpkg.image.verify_chunk_body",
            undefined,
            "After writing, the image is read back and checked. 8 MiB was fastest in measurement; try another if your disk disagrees. The check itself is the same at any size.",
          )}
        </span>
      </span>
      <select
        value={kib}
        onChange={(e) => set(Number(e.target.value))}
        className="select w-auto!"
        aria-label={tr("fpkg.image.verify_chunk", undefined, "Read-back check, per read")}
      >
        {VERIFY_CHUNK_KIB.map((k) => (
          <option key={k} value={k}>
            {k === DEFAULT_VERIFY_CHUNK_KIB
              ? tr("fpkg.image.verify_chunk_default", { n: k / 1024 }, "{n} MiB (default)")
              : tr("fpkg.image.verify_chunk_mib", { n: k / 1024 }, "{n} MiB")}
          </option>
        ))}
      </select>
    </label>
  );
}
