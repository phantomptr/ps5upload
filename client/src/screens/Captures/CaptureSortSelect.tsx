import { useTr } from "../../state/lang";
import type { CaptureSort } from "./sortCaptures";

export function CaptureSortSelect({ value, onChange }: {
  value: CaptureSort;
  onChange: (value: CaptureSort) => void;
}) {
  const tr = useTr();
  return (
    <label className="flex min-w-0 max-w-full items-center gap-1.5 text-xs text-[var(--color-muted)]">
      <span className="shrink-0">{tr("collection.sort", undefined, "Sort")}</span>
      <select
        className="select w-auto! min-w-0 max-w-full text-xs"
        value={value}
        onChange={(e) => onChange(e.target.value as CaptureSort)}
        aria-label={tr("collection.sort", undefined, "Sort")}
      >
        <option value="newest">{tr("captures_sort_newest", undefined, "Date: Newest first")}</option>
        <option value="oldest">{tr("captures_sort_oldest", undefined, "Date: Oldest first")}</option>
        <option value="smallest">{tr("collection.sort.size_asc", undefined, "Size (smallest first)")}</option>
        <option value="biggest">{tr("collection.sort.size_desc", undefined, "Size (largest first)")}</option>
      </select>
    </label>
  );
}
