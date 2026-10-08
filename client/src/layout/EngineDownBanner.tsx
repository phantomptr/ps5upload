import { EngineProblem } from "../components/EngineProblem";
import { useConnectionStore } from "../state/connection";

/** Shown on every screen while the app cannot reach its engine: why, and Restart engine. */
export default function EngineDownBanner() {
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  if (engineStatus !== "down") return null;
  return (
    <div
      className="border-b border-[var(--color-border)] bg-[var(--color-bad-soft)] px-3 py-2 text-[var(--color-text)]"
      role="alert"
    >
      <div className="mx-auto max-w-6xl">
        <EngineProblem compact />
      </div>
    </div>
  );
}
