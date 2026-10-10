import { EngineProblem } from "../components/EngineProblem";
import { useConnectionStore } from "../state/connection";

/** Shown on every screen while the app cannot reach its engine: why, and Restart engine. */
export default function EngineDownBanner() {
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  if (engineStatus !== "down") return null;
  return (
    <div
      className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-bad)_35%,transparent)] bg-[var(--color-bad-soft)] px-4 py-2.5 backdrop-blur-xl md:mx-5 text-[var(--color-text)]"
      role="alert"
    >
      <div className="mx-auto max-w-6xl">
        <EngineProblem compact />
      </div>
    </div>
  );
}
