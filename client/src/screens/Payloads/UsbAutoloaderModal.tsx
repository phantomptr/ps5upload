import { useEffect, useState } from "react";
import {
  HardDrive,
  RefreshCw,
  CheckCircle2,
  FilePlus,
  X,
} from "lucide-react";
import { pickPaths } from "../../lib/pickPath";
import {
  usbListRemovable,
  usbAutoloaderInstall,
  type UsbDrive,
  type PayloadInfo,
  type PayloadLocalEntry,
  type AutoloaderInstallResult,
} from "../../api/ps5";
import { Button, ErrorCard, Modal, Spinner, Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { catalogText } from "../../lib/payloadCatalogText";
import { payloadErrorText, usbSkippedText } from "../../lib/payloadErrors";
import { formatBytes } from "../../lib/format";
import { isMobile } from "../../lib/platform";

/**
 * USB autoloader wizard. Three-step inline modal:
 *
 *   1. Pick a removable drive (list comes from `usbListRemovable`).
 *   2. Pick which payloads to bundle: copies already in the app's
 *      library, and payload files from this computer (the app doesn't
 *      download payloads, so a new user's library is empty).
 *   3. Confirm + write. The Rust side reorders by autoload_priority,
 *      copies ELFs, and emits autoload.txt.
 *
 * No state machine — the steps are visually stacked + the user can
 * jump backwards by re-picking a drive. Keeps the surface area small
 * and matches the "scroll-down checklist" feel of the Connection
 * screen.
 */
export default function UsbAutoloaderModal({
  catalog,
  inventoryById,
  onClose,
}: {
  catalog: PayloadInfo[];
  inventoryById: Record<string, PayloadLocalEntry>;
  onClose: () => void;
}) {
  const tr = useTr();
  const [drives, setDrives] = useState<UsbDrive[] | null>(null);
  const [drivesError, setDrivesError] = useState<string | null>(null);
  const [pickedDrive, setPickedDrive] = useState<string | null>(null);
  const [pickedIds, setPickedIds] = useState<Set<string>>(new Set());
  const [includeUs, setIncludeUs] = useState(true);
  // Payload files from this computer, in the order added.
  const [localFiles, setLocalFiles] = useState<string[]>([]);
  const [installing, setInstalling] = useState(false);
  const [result, setResult] = useState<AutoloaderInstallResult | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);
  // Mobile: `usb_list_removable` deliberately returns an empty list
  // (there's no removable-drive mount enumeration on Android/iOS), so
  // the wizard would just show a bare "no drives" scan forever. Show a
  // clear "use the desktop app" empty state instead. UA-based and
  // stable for the session, so the unconditional hooks above are safe.
  const mobile = isMobile();

  useEffect(() => {
    if (mobile) return;
    refresh();
  }, [mobile]);

  async function refresh() {
    setDrives(null);
    setDrivesError(null);
    try {
      const d = await usbListRemovable();
      setDrives(d);
    } catch (e) {
      setDrivesError(String(e));
      // Clear the loading sentinel (drives === null) so the "Scanning…"
      // spinner stops and the Rescan button (disabled while null) re-enables.
      // Otherwise a failed scan wedges the wizard: endless spinner + no retry.
      setDrives([]);
    }
  }

  function togglePayload(id: string) {
    setPickedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function addLocalFiles() {
    // pickPaths: real paths on every platform (Android's dialog would hand back content:// URIs).
    const paths = await pickPaths({
      filters: [{ name: tr("usb_wizard_payload_filter", undefined, "Payload"), extensions: ["elf", "bin"] }],
    }).catch(() => [] as string[]);
    if (paths.length === 0) return;
    setLocalFiles((prev) => [...prev, ...paths.filter((p) => !prev.includes(p))]);
    setResult(null);
  }

  async function handleInstall() {
    if (!pickedDrive) return;
    setInstalling(true);
    setInstallError(null);
    setResult(null);
    try {
      const r = await usbAutoloaderInstall({
        drive_path: pickedDrive,
        payload_ids: Array.from(pickedIds),
        include_ps5upload: includeUs,
        local_files: localFiles,
      });
      setResult(r);
    } catch (e) {
      setInstallError(payloadErrorText(tr, e));
    } finally {
      setInstalling(false);
    }
  }

  return (
    <Modal
      open
      onClose={onClose}
      title={tr("usb_wizard_title_v2", undefined, "USB payload stick")}
      titleIcon={<HardDrive size={16} />}
      size="xl"
    >
      <div className="space-y-5 p-5 text-sm">
        {mobile ? (
          <div className="flex flex-col items-center gap-2 rounded-[var(--radius-card)] border border-dashed border-[var(--color-border)] p-6 text-center">
            <HardDrive size={24} className="text-[var(--color-muted)]" />
            <div className="text-sm font-medium">
              {tr(
                "usb_wizard_mobile_unavailable",
                undefined,
                "USB drive preparation requires the desktop app (Windows/macOS/Linux).",
              )}
            </div>
            <div className="text-xs text-[var(--color-muted)]">
              {tr(
                "usb_wizard_mobile_unavailable_hint",
                undefined,
                "This wizard writes payloads onto a USB stick plugged into your computer — there's no removable-drive access on this device.",
              )}
            </div>
          </div>
        ) : (
          <>
            {/* Step 1 — drive */}
            <section>
              <SectionHeader
                index={1}
                title={tr(
                  "usb_wizard_step1",
                  undefined,
                  "Pick a removable drive",
                )}
                right={
                  <Button
                    variant="ghost"
                    size="sm"
                    leftIcon={<RefreshCw size={11} />}
                    onClick={refresh}
                    disabled={drives === null}
                  >
                    {tr("usb_wizard_rescan", undefined, "Rescan")}
                  </Button>
                }
              />
              {drivesError && <ErrorCard title={drivesError} />}
              {!drives ? (
                <div className="text-xs text-[var(--color-muted)]">
                  <Spinner size={12} className="me-2 inline" />
                  {tr("usb_wizard_scanning", undefined, "Scanning…")}
                </div>
              ) : drives.length === 0 ? (
                <div className="rounded-[var(--radius-card)] border border-dashed border-[var(--color-border)] p-3 text-xs text-[var(--color-muted)]">
                  {tr(
                    "usb_wizard_no_drives",
                    undefined,
                    "No removable drives detected. Plug in a USB stick formatted as exFAT or FAT32 and click Rescan.",
                  )}
                </div>
              ) : (
                <ul className="space-y-1.5">
                  {drives.map((d) => (
                    <li
                      key={d.path}
                      className={`flex items-center gap-3 rounded-[var(--radius-field)] border p-2 text-xs ${
                        pickedDrive === d.path
                          ? "border-[var(--color-accent)] bg-[var(--color-surface)]"
                          : "border-[var(--color-border)]"
                      }`}
                    >
                      <input
                        type="radio"
                        name="drive"
                        checked={pickedDrive === d.path}
                        onChange={() => {
                          setPickedDrive(d.path);
                          // The previous drive's success/error result no longer
                          // applies to the newly selected drive — clear it so
                          // the user doesn't see a stale "installed" message.
                          setResult(null);
                          setInstallError(null);
                        }}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="font-medium">{d.label}</div>
                        <div className="text-xs text-[var(--color-muted)]">
                          {d.path}
                        </div>
                      </div>
                      <div className="text-end text-xs text-[var(--color-muted)]">
                        {formatBytes(d.free_bytes)}{" "}
                        {tr("usbmodal_free_slash", "free /")}{" "}
                        {formatBytes(d.total_bytes)}
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </section>

            {/* Step 2 — payloads */}
            {pickedDrive && (
              <section>
                <SectionHeader
                  index={2}
                  title={tr(
                    "usb_wizard_step2",
                    undefined,
                    "Pick payloads to bundle",
                  )}
                />
                <p className="mb-2 text-xs text-[var(--color-muted)]">
                  {tr(
                    "usb_wizard_step2_hint_v2",
                    undefined,
                    "Tick copies already in the app's library, or add payload files from this computer.",
                  )}
                </p>
                <div className="mb-3">
                  <Button
                    variant="secondary"
                    size="sm"
                    leftIcon={<FilePlus size={12} />}
                    onClick={() => void addLocalFiles()}
                  >
                    {tr("usb_wizard_add_files", undefined, "Add files from this computer…")}
                  </Button>
                  {localFiles.length > 0 && (
                    <ul className="mt-2 space-y-1">
                      {localFiles.map((f) => (
                        <li
                          key={f}
                          className="flex items-center gap-3 rounded-[var(--radius-field)] border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs"
                        >
                          <div className="min-w-0 flex-1">
                            <div className="truncate font-medium">{f.split(/[\\/]/).pop()}</div>
                            <div className="truncate text-xs text-[var(--color-muted)]" title={f}>
                              {f}
                            </div>
                          </div>
                          <Button
                            variant="ghost"
                            size="sm"
                            aria-label={tr("usb_wizard_remove_file", undefined, "Remove")}
                            leftIcon={<X size={12} />}
                            onClick={() => setLocalFiles((prev) => prev.filter((x) => x !== f))}
                          />
                        </li>
                      ))}
                    </ul>
                  )}
                  <p className="mt-1 text-xs text-[var(--color-muted)]">
                    {tr(
                      "usb_wizard_local_order",
                      undefined,
                      "Your files load after the library's payloads, in the order added.",
                    )}
                  </p>
                </div>
                <ul className="space-y-1">
                  {catalog.map((p) => {
                    const cached = !!inventoryById[p.id];
                    const checked = pickedIds.has(p.id);
                    return (
                      <li
                        key={p.id}
                        className={`flex items-center gap-3 rounded-[var(--radius-field)] border p-2 text-xs ${
                          cached
                            ? checked
                              ? "border-[var(--color-accent)] bg-[var(--color-surface)]"
                              : "border-[var(--color-border)]"
                            : "border-dashed border-[var(--color-border)] opacity-60"
                        }`}
                      >
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={() => togglePayload(p.id)}
                          disabled={!cached}
                        />
                        <div className="min-w-0 flex-1">
                          <div className="font-medium">{p.display_name}</div>
                          <div className="text-xs text-[var(--color-muted)]">
                            {catalogText(tr, p).role} {tr("usbmodal_priority", "· priority")}{" "}
                            {p.autoload_priority}
                          </div>
                        </div>
                        {cached && inventoryById[p.id]?.version && (
                          <span className="text-xs text-[var(--color-muted)]">
                            {inventoryById[p.id].version}
                          </span>
                        )}
                        {!cached && (
                          <span className="text-xs text-[var(--color-muted)]">
                            {tr(
                              "usb_wizard_not_cached",
                              undefined,
                              "not cached",
                            )}
                          </span>
                        )}
                      </li>
                    );
                  })}
                </ul>
                <Toggle
                  checked={includeUs}
                  onChange={(checked) => setIncludeUs(checked)}
                  label={tr(
                    "usb_wizard_include_us",
                    undefined,
                    "Also include ps5upload.elf so the stick boots the desktop tool's payload too",
                  )}
                  className="mt-3 text-xs"
                />
              </section>
            )}

            {/* Step 3 — install + result */}
            {pickedDrive && (pickedIds.size > 0 || localFiles.length > 0 || includeUs) && (
              <section>
                <SectionHeader
                  index={3}
                  title={tr("usb_wizard_step3", undefined, "Write to drive")}
                />
                <Button
                  variant="primary"
                  size="md"
                  leftIcon={
                    installing ? (
                      <Spinner size={12} tone="inherit" />
                    ) : (
                      <CheckCircle2 size={12} />
                    )
                  }
                  onClick={handleInstall}
                  disabled={installing}
                >
                  {installing
                    ? tr("usb_wizard_writing", undefined, "Writing…")
                    : tr(
                        "usb_wizard_install",
                        undefined,
                        "Write ps5_autoloader/ to drive",
                      )}
                </Button>
                {installError && (
                  <div className="mt-2">
                    <ErrorCard title={installError} />
                  </div>
                )}
                {result && (
                  <div className="mt-2 space-y-2 text-xs">
                    <div className="rounded-[var(--radius-card)] border border-[var(--color-good)] bg-[var(--color-surface)] p-3">
                      <div className="font-medium text-[var(--color-good)]">
                        {tr(
                          "usb_wizard_done",
                          { count: result.written.length },
                          `Wrote ${result.written.length} files`,
                        )}
                      </div>
                      <ul className="mt-1 list-disc ps-4 text-[var(--color-muted)]">
                        {result.written.map((w) => (
                          <li key={w}>{w}</li>
                        ))}
                      </ul>
                    </div>
                    {result.skipped.length > 0 && (
                      <div className="rounded-[var(--radius-card)] border border-[var(--color-warn)] bg-[var(--color-surface)] p-3">
                        <div className="font-medium text-[var(--color-warn)]">
                          {tr("usb_wizard_skipped", undefined, "Skipped")}
                        </div>
                        <ul className="mt-1 list-disc ps-4 text-[var(--color-muted)]">
                          {result.skipped.map((s) => (
                            <li key={s}>{usbSkippedText(tr, s)}</li>
                          ))}
                        </ul>
                      </div>
                    )}
                    <details className="rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
                      <summary className="cursor-pointer text-[var(--color-muted)]">
                        {tr(
                          "usb_wizard_show_autoload_txt",
                          undefined,
                          "Generated autoload.txt",
                        )}
                      </summary>
                      <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-[var(--color-text)]">
                        {result.autoload_txt}
                      </pre>
                    </details>
                  </div>
                )}
              </section>
            )}
          </>
        )}
      </div>
    </Modal>
  );
}

function SectionHeader({
  index,
  title,
  right,
}: {
  index: number;
  title: string;
  right?: React.ReactNode;
}) {
  return (
    <div className="mb-2 flex items-center justify-between">
      <h3 className="flex items-center gap-2 text-sm font-semibold">
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-[var(--color-surface-3)] text-xs tabular-nums">
          {index}
        </span>
        {title}
      </h3>
      {right}
    </div>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary).
