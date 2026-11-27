import { Callout } from "../../../components/Callout";
import { useTr } from "../../../state/lang";

/** A file or process name that is kstuff ("kstuff.elf", "kstuff-1.15-fpkg.elf"). */
export function isKstuffName(name: string | null | undefined): boolean {
  if (!name) return false;
  const base = name.split(/[\\/]/).pop() ?? "";
  return /kstuff/i.test(base);
}

/** Newer kstuff builds stay in the kernel and leave no running program, so the only sign is
 *  the console's kernel access. That can come from other kernel payloads too, so this is a
 *  "probably", never proof. True when kernel access is reported and no kstuff process runs. */
export function kstuffProbablyInKernel(
  processes: readonly { comm: string; name: string; is_self?: boolean }[],
  ucredElevated: boolean | null,
): boolean {
  if (ucredElevated !== true) return false;
  return !processes.some((p) => !p.is_self && (isKstuffName(p.comm) || isKstuffName(p.name)));
}

/** The explanation shown on the Payloads page and when sending a kstuff file. */
export function KstuffKernelNote({ variant }: { variant: "hub" | "send" }) {
  const tr = useTr();
  return (
    <Callout
      tone="info"
      title={
        variant === "hub"
          ? tr("kstuff_kernel_title", undefined, "kstuff: probably active in the kernel")
          : tr("kstuff_kernel_send_title", undefined, "kstuff may already be active")
      }
    >
      <p data-testid={`kstuff-kernel-${variant}`}>
        {tr(
          "kstuff_kernel_body",
          undefined,
          "This PS5 reports kernel access, but no kstuff program is running. Newer kstuff builds stay in the kernel without a running program, so they don't show as running and can't be stopped. Kernel access can also come from other payloads, so this isn't certain: if a fake package installs and starts, kstuff is active.",
        )}
      </p>
      <p className="mt-1">
        {tr(
          "kstuff_kernel_change",
          undefined,
          "Sending kstuff again does nothing while one is active. To change kstuff, restart the console and send the one you want first.",
        )}
      </p>
    </Callout>
  );
}
