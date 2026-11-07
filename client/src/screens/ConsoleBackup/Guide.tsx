import { useTr } from "../../state/lang";
import { Card } from "../../components";
import { openExternalUrl } from "../../lib/openExternalUrl";

const SONY_SAFE_MODE = "https://www.playstation.com/support/hardware/safe-mode-playstation/";

/** When a console won't start, in the order to try things: each step costs more than the one
 *  before it. Sony's own page has the options; the rest is what a jailbroken console changes. */
export function ResetGuide() {
  const tr = useTr();
  const steps: { title: string; body: string; erases?: boolean }[] = [
    {
      title: tr("cb_step_restart", undefined, "Restart, and go into Safe Mode again"),
      body: tr(
        "cb_step_restart_body",
        undefined,
        "Safe Mode can open offering only the last option, Reset PS5 (Reinstall System Software). People report that choosing Restart PS5 and going into Safe Mode once more brings the other options back. Try this before anything else.",
      ),
    },
    {
      title: tr("cb_step_repair", undefined, "Repair Console Storage"),
      body: tr(
        "cb_step_repair_body",
        undefined,
        "Checks the drive and repairs it. It erases nothing, so it's the first real option to try.",
      ),
    },
    {
      title: tr("cb_step_rebuild", undefined, "Clear Cache and Rebuild Database"),
      body: tr(
        "cb_step_rebuild_body",
        undefined,
        "Rebuilds the console's list of content. It keeps your games and saves. On a jailbroken console, games and apps that were registered by hand may need registering again. Some people report this was enough, but we haven't confirmed that.",
      ),
    },
    {
      title: tr("cb_step_defaults", undefined, "Restore Default Settings"),
      body: tr(
        "cb_step_defaults_body",
        undefined,
        "Puts the settings back to their defaults and keeps your games and saves.",
      ),
    },
    {
      title: tr("cb_step_reset", undefined, "Reset PS5"),
      erases: true,
      body: tr(
        "cb_step_reset_body",
        undefined,
        "Erases users, games and saves. It leaves the installed system software alone, so your firmware doesn't change and you don't need a USB drive. If the options above didn't help, this is the one to use.",
      ),
    },
    {
      title: tr("cb_step_reinstall", undefined, "Reset PS5 (Reinstall System Software)"),
      erases: true,
      body: tr(
        "cb_step_reinstall_body",
        undefined,
        "The last resort: it also removes the system software and needs a reinstallation file on a USB drive. Sony's site only offers the newest version, so using its file updates your firmware and can end your jailbreak.",
      ),
    },
  ];
  const usb = [
    tr("cb_usb_format", undefined, "Format the drive as FAT32 or exFAT."),
    tr("cb_usb_folders", undefined, "Make a folder named PS5, and inside it a folder named UPDATE, in capital letters."),
    tr("cb_usb_file", undefined, "Save the file there as PS5UPDATE.PUP. It must be the reinstallation file, which is much larger than the update file."),
    tr("cb_usb_refused", undefined, "If the PS5 still says it can't find the file, check those three things, then try another drive: some drives don't work."),
  ];
  const after = [
    tr("cb_after_offline", undefined, "Set the PS5 up without connecting it to the internet, and decline any system update it offers. An update can end your jailbreak."),
    tr("cb_after_user", undefined, "Create your users again. Accounts has the steps for giving a new user their old account ID."),
    tr("cb_after_jailbreak", undefined, "Jailbreak the console again and send the helper, then open this screen and restore your backup."),
  ];
  return (
    <div className="space-y-4" data-testid="cb-guide">
      <Card>
        <h2 className="text-sm font-semibold">{tr("cb_enter_title", undefined, "Getting into Safe Mode")}</h2>
        <p className="mt-1 text-sm text-[var(--color-muted)]">
          {tr(
            "cb_enter_body",
            undefined,
            "Turn the PS5 off. Hold the power button until you hear the second beep, about seven seconds. Connect the controller with a USB cable and press the PS button.",
          )}
        </p>
      </Card>
      <Card>
        <h2 className="text-sm font-semibold">{tr("cb_steps_title", undefined, "What to try, in this order")}</h2>
        <p className="mt-1 text-sm text-[var(--color-muted)]">
          {tr("cb_steps_hint", undefined, "Each step costs more than the one before. Stop as soon as the PS5 starts.")}
        </p>
        <ol className="mt-3 space-y-3">
          {steps.map((s, i) => (
            <li key={s.title} className="flex gap-3">
              <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[var(--color-surface-3)] text-xs font-semibold">
                {i + 1}
              </span>
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
                  {s.title}
                  {s.erases && (
                    <span className="rounded-full bg-[var(--color-bad-soft)] px-2 py-0.5 text-[0.6875rem] font-medium text-[var(--color-bad)]">
                      {tr("cb_erases", undefined, "Erases your data")}
                    </span>
                  )}
                </div>
                <p className="mt-0.5 text-sm text-[var(--color-muted)]">{s.body}</p>
              </div>
            </li>
          ))}
        </ol>
        <button
          type="button"
          className="mt-3 text-xs text-[var(--color-accent-bright)] underline"
          onClick={() => void openExternalUrl(SONY_SAFE_MODE)}
        >
          {tr("cb_sony_link", undefined, "Sony's Safe Mode page")}
        </button>
      </Card>
      <Card>
        <h2 className="text-sm font-semibold">{tr("cb_usb_title", undefined, "If you need the USB drive")}</h2>
        <ul className="mt-2 list-disc space-y-1 ps-5 text-sm text-[var(--color-muted)]">
          {usb.map((t) => (
            <li key={t}>{t}</li>
          ))}
        </ul>
      </Card>
      <Card>
        <h2 className="text-sm font-semibold">{tr("cb_after_title", undefined, "After a reset")}</h2>
        <ol className="mt-2 list-decimal space-y-1 ps-5 text-sm text-[var(--color-muted)]">
          {after.map((t) => (
            <li key={t}>{t}</li>
          ))}
        </ol>
      </Card>
    </div>
  );
}
