import { Spinner } from "../components/Spinner";
import { useTr } from "../state/lang";
import { useUpdateStore } from "../state/update";

/**
 * The app was opened again while an update from the last quit is still replacing it. It waits
 * for that to end and then starts again as the new version (state/update `finishingUpdate`);
 * until then this small card is all there is to do. The wait is bounded: if the install is
 * still working after a few minutes the card goes and the app carries on as it is.
 */
export function UpdateFinishing() {
  const finishing = useUpdateStore((s) => s.finishingUpdate);
  return finishing ? <UpdateFinishingCard /> : null;
}

export function UpdateFinishingCard() {
  const tr = useTr();
  return (
    <div
      role="status"
      aria-live="polite"
      data-testid="update-finishing"
      className="scrim fixed inset-0 z-[90] flex items-center justify-center p-4"
    >
      <div className="flex max-w-sm items-center gap-3 rounded-2xl border border-[var(--color-border)] bg-[var(--color-surface)] px-5 py-4 shadow-xl">
        <Spinner size={18} />
        <div className="min-w-0">
          <div className="font-semibold text-[var(--color-text)]">
            {tr("update_finishing_title", undefined, "Finishing the update…")}
          </div>
          <div className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr("update_finishing_body", undefined, "PS5Upload starts again by itself when it is done.")}
          </div>
        </div>
      </div>
    </div>
  );
}

export default UpdateFinishing;
