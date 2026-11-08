import { Button, Toggle } from "../../components";
import type { AmprLz4Options, AmprPacksReadiness, FfpfscLevel, ImageFormat, ZstdPacks } from "../../api/fpkg";
import {
  DEFAULT_VERIFY_CHUNK_KIB,
  VERIFY_CHUNK_KIB,
  parseAmprEmuBuild,
  parseZstdRuntime,
  useConvertPrefs,
  type AmprEmuBuild,
} from "../../state/convertPrefs";
import { useTr } from "../../state/lang";

// The options of a game image (③ Options when "A game image" is what Convert makes): its
// filesystem, .ffpfsc compression, AMPR LZ4 asset packs for a libSceAmpr game, and how the
// read-back check reads.

/** The AMPR LZ4 asset-pack option for a libSceAmpr game's .exfat image. */
export interface Lz4Choice {
  /** What the check found about the game's own ampr_emu. */
  readiness: AmprPacksReadiness;
  on: boolean;
  onToggle: (on: boolean) => void;
  /** A profile read from a .toml file (its name); null packs with the built-in one. */
  profileName: string | null;
  onProfile: (profile: { name: string; text: string } | null) => void;
  /** Install ampr_emu 0.4.2.1 into the image (offered when `readiness.kind` is `runtime`). */
  installRuntime: boolean;
  onInstallRuntime: (on: boolean) => void;
  /** Which packs: LZ4 (plain exFAT only, the game's own emulator) or zstd (any plain image,
   *  with ampr-emu-ng). */
  kind: "lz4" | "zstd";
  onKind: (kind: "lz4" | "zstd") => void;
  zstdLevel: number;
  onZstdLevel: (level: number) => void;
}

/** The zstd levels offered, as ps5-dump-forge offers them. */
const ZSTD_LEVELS = [1, 5, 19];

/** Packs are refused for this game unless the engine installs the runtime, which is on. */
export function lz4Refused(lz4: Lz4Choice): string | null {
  if (!lz4.readiness.refusal) return null;
  // zstd packs bring their own emulator: only a game that doesn't read through AMPR is out.
  if (lz4.kind === "zstd" && lz4.readiness.kind !== "no_ampr") return null;
  if (lz4.readiness.kind === "runtime" && lz4.installRuntime) return null;
  return lz4.readiness.refusal;
}

/** What the build is told when packs are on, or nothing. Packing replaces .ffpfsc compression:
 *  the packs are already compressed, so the image holding them is a plain one. */
export function packOptions(
  format: ImageFormat,
  lz4: Lz4Choice | undefined,
  profileToml?: string,
): AmprLz4Options | undefined {
  if (!lz4 || !lz4Active(format, lz4)) return undefined;
  return lz4.kind === "zstd"
    ? { zstdLevel: lz4.zstdLevel, zstdRuntime: useConvertPrefs.getState().zstdRuntime }
    : { profileToml, installRuntime: lz4.installRuntime };
}

/** Whether a pack format is on for this image. */
export function lz4Active(format: ImageFormat, lz4: Lz4Choice | undefined): boolean {
  return !!lz4?.on && (format === "exfat" || lz4.kind === "zstd") && !lz4Refused(lz4);
}

/** Whether a folder build packs: zstd only (LZ4 packs go into an .exfat image). */
export function folderPacks(lz4: Lz4Choice | undefined): boolean {
  return !!lz4?.on && lz4.kind === "zstd" && !lz4Refused(lz4);
}

/** What a folder build is told when zstd packs are on, or nothing. */
export function folderPackOptions(lz4: Lz4Choice | undefined): ZstdPacks | undefined {
  return lz4 && folderPacks(lz4)
    ? { level: lz4.zstdLevel, runtime: useConvertPrefs.getState().zstdRuntime }
    : undefined;
}

/** The tested ampr-emu-ng release, as the engine pins it (zstd_packs.rs). */
const NG_TESTED = "0.3.2";

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
                  "fpkg.folder.zstd_body",
                  undefined,
                  "Writes the game as a plain folder with all but its system files in zstd packs, and quer3q's ampr-emu-ng as fakelib/libSceAmpr.sprx. A game folder can be the source too. LZ4 packs are not offered here: they go into an .exfat image only. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read them.",
                )
              : tr(
                  "fpkg.image.zstd_body",
                  undefined,
                  "Packs all but the game's system files into zstd packs, written straight into the image with no temporary copy. The image gets quer3q's ampr-emu-ng as its emulator, checked against a published digest before it is used. A game already in LZ4 packs is switched over. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read them.",
                )}
          </span>
        </span>
      </label>
      {on && <ZstdRuntimeChoice />}
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

function Lz4Option({ lz4 }: { lz4: Lz4Choice }) {
  const tr = useTr();
  const refusal = lz4Refused({ ...lz4, kind: "lz4" });
  const kind = lz4.readiness.kind ?? (lz4.readiness.refusal ? "runtime" : "ok");
  return (
    <div className="space-y-1" data-testid="lz4-option">
      <label className="flex cursor-pointer items-start gap-2.5 text-xs">
        <input
          type="checkbox"
          checked={lz4.on && lz4.kind === "lz4" && !refusal}
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
            {tr(
              "fpkg.image.lz4_body",
              undefined,
              "Packs the game's data into LZ4 asset packs that the game's own ampr_emu (fakelib/libSceAmpr.sprx) reads back, so the image is smaller. Executables, system files and already-compressed media stay as they are. Plain .exfat only, without .ffpfsc compression.",
            )}
          </span>
        </span>
      </label>
      {kind === "runtime" && (
        <label className="flex cursor-pointer items-start gap-2.5 text-xs" data-testid="lz4-install-runtime">
          <input
            type="checkbox"
            checked={lz4.installRuntime}
            onChange={(e) => lz4.onInstallRuntime(e.target.checked)}
            className="checkbox mt-px h-4 w-4 shrink-0"
          />
          <span className="min-w-0">
            <span className="block font-medium text-[var(--color-text)]">
              {tr("fpkg.image.lz4_install_runtime", undefined, "Put ampr_emu 0.4.2.1 into the image")}
            </span>
            <span className="mt-0.5 block leading-relaxed text-[var(--color-muted)]">
              {tr(
                "fpkg.image.lz4_install_runtime_body",
                { reason: lz4.readiness.refusal ?? "" },
                "{reason}. The image gets drakmor's ampr_emu 0.4.2.1 as fakelib/libSceAmpr.sprx (fetched once from its GitHub release and checked against a known digest); the game's own copy stays beside it as libSceAmpr.sprx.orig.",
              )}
            </span>
          </span>
        </label>
      )}
      {kind === "no_ampr" ? (
        <div className="text-xs text-[var(--color-muted)]">
          {tr(
            "fpkg.image.lz4_no_ampr",
            undefined,
            "This game does not read its files through AMPR, so nothing in it could serve asset packs. Compress it into a .ffpfsc instead: ShadowMount+ decompresses that for any game.",
          )}
        </div>
      ) : refusal && kind === "runtime" ? (
        // The checkbox above fixes this one: say so, rather than that packs are unavailable (#449).
        <div className="text-xs text-[var(--color-warn)]">
          {tr(
            "fpkg.image.lz4_needs_runtime",
            { reason: refusal },
            "Tick “Put ampr_emu 0.4.2.1 into the image” above to use packs: {reason}",
          )}
        </div>
      ) : refusal ? (
        <div className="text-xs text-[var(--color-warn)]">
          {tr("fpkg.image.lz4_unavailable", { reason: refusal }, "Not available for this game: {reason}")}
        </div>
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
      {lz4.on && lz4.kind === "lz4" && !refusal && (
        <div className="text-xs text-[var(--color-muted)]" data-testid="lz4-placement-note">
          {tr(
            "fpkg.image.lz4_placement_note",
            undefined,
            "Keep a packed image on the console's internal storage: one has been reported to hang on a black screen from USB. Turn off update_emulators in ShadowMount+, or it can swap the emulator for one that can't read packs.",
          )}
        </div>
      )}
      {lz4.on && lz4.kind === "lz4" && !refusal && (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          {lz4.profileName ? (
            <>
              <span className="text-[var(--color-muted)]">
                {tr("fpkg.image.lz4_profile_named", { name: lz4.profileName }, "Profile: {name}")}
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
        </div>
      )}
    </div>
  );
}

/** Some libSceAmpr games with no emulator fail to start from an image (Alembic's two on a console),
 *  others run (LEGO Horizon Adventures reaches its menu with or without one, on the Pro), and
 *  ShadowMount+ doesn't add one. On by default; packs have their own choice. */
function AmprEmuOption() {
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
          "fpkg.image.ampr_emu_body",
          undefined,
          "This game reads its files through AMPR and has no emulator. Some such games won't start from an image without one, and others run fine. Adds drakmor's ampr_emu 0.4.2.1 as fakelib/libSceAmpr.sprx, fetched once from its GitHub release and checked against a known digest.",
        )}
      />
      {on && <AmprEmuBuildChoice build={build} onBuild={setBuild} />}
    </div>
  );
}

/** Product names, the same in every language. */
const AMPR_EMU = "ampr_emu 0.4.2.1";
const AMPR_EMU_NG = "ampr-emu-ng 0.3.2";

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
}: {
  format: ImageFormat;
  compress: boolean;
  onChoice: (format: ImageFormat, compress: boolean) => void;
  lz4?: Lz4Choice;
  /** One game's own compression level (a game in the queue); else the remembered one. */
  level?: { value: FfpfscLevel; onChange: (level: FfpfscLevel) => void };
}) {
  const tr = useTr();
  const setFormat = (f: ImageFormat) => onChoice(f, compress);
  const setCompress = (c: boolean) => onChoice(format, c);
  const packing = lz4Active(format, lz4);
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
          checked={compress && !packing}
          disabled={packing}
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
      {compress && !packing && <FfpfscLevelOption own={level} />}
      {lz4?.readiness.emulator_missing && !packing && <AmprEmuOption />}
      {lz4 && format === "exfat" && <Lz4Option lz4={lz4} />}
      {lz4 && lz4.readiness.kind !== "no_ampr" && <ZstdOption lz4={lz4} />}
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
