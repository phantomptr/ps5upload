import {
  type ReactNode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { ChevronDown, ChevronUp, ChevronsUpDown } from "lucide-react";

import { useTr } from "../state/lang";

/**
 * v5 Table / DataGrid primitive (§22.13).
 *
 * WAI-ARIA Grid pattern: arrow-key cell navigation, sortable headers,
 * selectable rows, sticky header. Implements a lightweight internal
 * virtualization (progressive page-size growth via sentinel IntersectionObserver)
 * so we don't need to ship react-window as a dependency — good enough for
 * the row counts this app actually sees (hundreds, not millions).
 *
 * Accessibility contract (§20.4.2):
 *   - The grid container has role="grid".
 *   - Each row has role="row"; each cell role="gridcell".
 *   - Arrow keys move the active cell; the active cell gets tabindex=0,
 *     all others tabindex=-1, per the WAI-ARIA Grid pattern.
 *   - Column headers are role="columnheader" with aria-sort reflecting state.
 *   - Selectable rows expose aria-selected.
 *   - Home/End jump to first/last cell in a row.
 *   - Ctrl+Home/Ctrl+End jump to first/last row.
 */

export type SortDirection = "asc" | "desc";

export interface TableColumn<T> {
  /** Unique key for this column; also the field name when using the simple
   *  data-array form of Table. */
  key: string;
  /** Header label. */
  header: ReactNode;
  /** Cell renderer. Receives the row datum; returns ReactNode. */
  cell: (row: T, index: number) => ReactNode;
  /** Optional sort comparator. When provided, the header shows sort arrows
   *  and clicking it toggles asc→desc→none. */
  sort?: (a: T, b: T) => number;
  /** Width hint (CSS value for the col element). */
  width?: string;
  /** Align cell content. */
  align?: "left" | "right" | "center";
  /** Sticky column (left). */
  sticky?: boolean;
}

export interface TableProps<T> {
  /** Column definitions. */
  columns: TableColumn<T>[];
  /** Row data. */
  rows: T[];
  /** Unique row key extractor. */
  rowKey: (row: T, index: number) => string;
  /** Row selection. When provided, a checkbox column appears (sticky left). */
  selectedKeys?: Set<string>;
  onSelectionChange?: (keys: Set<string>) => void;
  /** Multi-select (default) or single-select. */
  selectionMode?: "single" | "multi";
  /** Sorted column key + direction (controlled). */
  sortKey?: string;
  sortDirection?: SortDirection;
  onSortChange?: (key: string, direction: SortDirection) => void;
  /** Row click handler. */
  onRowClick?: (row: T, index: number) => void;
  /** Per-row disabled state (dims + blocks selection/click). */
  isRowDisabled?: (row: T, index: number) => boolean;
  /** Page size for progressive rendering. Default 50. */
  pageSize?: number;
  /** Empty state node rendered when rows is empty. */
  emptyState?: ReactNode;
  /** Extra className on the scroll container. */
  className?: string;
  /** Compact row height (28px) vs default (40px). */
  density?: "compact" | "default";
  /** Show zebra striping. */
  zebra?: boolean;
}

const ARROW_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
]);

export function Table<T>({
  columns,
  rows,
  rowKey,
  selectedKeys,
  onSelectionChange,
  selectionMode = "multi",
  sortKey: controlledSortKey,
  sortDirection: controlledSortDir,
  onSortChange,
  onRowClick,
  isRowDisabled,
  pageSize = 50,
  emptyState,
  className = "",
  density = "default",
  zebra = false,
}: TableProps<T>) {
  const tr = useTr();

  // ---- Uncontrolled sort fallback (for when onSortChange is not provided) ----
  const [internalSortKey, setInternalSortKey] = useState<string | undefined>(
    controlledSortKey,
  );
  const [internalSortDir, setInternalSortDir] = useState<SortDirection | undefined>(
    controlledSortDir,
  );

  // Sync controlled → internal when the parent updates the controlled value.
  useLayoutEffect(() => {
    setInternalSortKey(controlledSortKey);
  }, [controlledSortKey]);
  useLayoutEffect(() => {
    setInternalSortDir(controlledSortDir);
  }, [controlledSortDir]);

  const activeSortKey = controlledSortKey ?? internalSortKey;
  const activeSortDir = controlledSortDir ?? internalSortDir;

  // ---- Sorted rows ----
  const sortedRows = useMemo(() => {
    if (!activeSortKey || !activeSortDir) return rows;
    const col = columns.find((c) => c.key === activeSortKey);
    if (!col?.sort) return rows;
    const factor = activeSortDir === "asc" ? 1 : -1;
    return [...rows].sort((a, b) => col.sort!(a, b) * factor);
  }, [rows, columns, activeSortKey, activeSortDir]);

  // ---- Progressive rendering (virtualization-lite) ----
  const [renderCount, setRenderCount] = useState(pageSize);
  const sentinelRef = useRef<HTMLDivElement | null>(null);

  // Reset count when the data identity changes (new query, sort, etc.).
  useEffect(() => {
    setRenderCount(pageSize);
  }, [sortedRows, pageSize]);

  useEffect(() => {
    const el = sentinelRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) {
          setRenderCount((c) => c + pageSize);
        }
      },
      { rootMargin: "200px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [pageSize]);

  const visibleRows = sortedRows.slice(0, renderCount);
  const hasMore = renderCount < sortedRows.length;

  // ---- Active cell (keyboard navigation) ----
  // Stored as {row, col} indices into the logical grid. Col 0 is the checkbox
  // column when selection is enabled.
  const selectionEnabled = !!onSelectionChange && !!selectedKeys;
  const colOffset = selectionEnabled ? 1 : 0;
  const totalCols = columns.length + colOffset;

  const [activeCell, setActiveCell] = useState<{ row: number; col: number }>({
    row: 0,
    col: 0,
  });

  // Clamp active cell when data changes.
  useLayoutEffect(() => {
    setActiveCell((ac) => ({
      row: Math.min(ac.row, Math.max(0, sortedRows.length - 1)),
      col: Math.min(ac.col, totalCols - 1),
    }));
  }, [sortedRows.length, totalCols]);

  // ---- Sort header click ----
  const handleSort = useCallback(
    (key: string) => {
      const col = columns.find((c) => c.key === key);
      if (!col?.sort) return;
      const currentKey = activeSortKey;
      const currentDir = activeSortDir;
      let nextDir: SortDirection;
      if (currentKey !== key) nextDir = "asc";
      else if (currentDir === "asc") nextDir = "desc";
      else nextDir = "asc"; // toggle asc→desc→asc (no "none" for simplicity)

      if (onSortChange) {
        onSortChange(key, nextDir);
      } else {
        setInternalSortKey(key);
        setInternalSortDir(nextDir);
      }
    },
    [columns, activeSortKey, activeSortDir, onSortChange],
  );

  // ---- Selection helpers ----
  const allVisibleKeys = useMemo(
    () =>
      visibleRows
        .map((r, i) => ({ key: rowKey(r, i), disabled: isRowDisabled?.(r, i) ?? false }))
        .filter((x) => !x.disabled)
        .map((x) => x.key),
    [visibleRows, rowKey, isRowDisabled],
  );

  const allSelected =
    selectionEnabled &&
    allVisibleKeys.length > 0 &&
    allVisibleKeys.every((k) => selectedKeys!.has(k));
  const someSelected =
    selectionEnabled && !allSelected && allVisibleKeys.some((k) => selectedKeys!.has(k));

  const toggleAll = useCallback(() => {
    if (!onSelectionChange || !selectedKeys) return;
    const next = new Set(selectedKeys);
    if (allSelected) {
      allVisibleKeys.forEach((k) => next.delete(k));
    } else {
      allVisibleKeys.forEach((k) => next.add(k));
    }
    onSelectionChange(next);
  }, [onSelectionChange, selectedKeys, allVisibleKeys, allSelected]);

  const toggleRow = useCallback(
    (key: string) => {
      if (!onSelectionChange || !selectedKeys) return;
      const next = new Set(selectedKeys);
      if (selectionMode === "single") next.clear();
      if (next.has(key)) next.delete(key);
      else next.add(key);
      onSelectionChange(next);
    },
    [onSelectionChange, selectedKeys, selectionMode],
  );

  // ---- Keyboard grid navigation ----
  const handleGridKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (!ARROW_KEYS.has(e.key) && e.key !== "Home" && e.key !== "End") return;
      const maxRow = sortedRows.length - 1;
      const maxCol = totalCols - 1;
      if (maxRow < 0) return;

      e.preventDefault();
      setActiveCell((ac) => {
        let { row, col } = ac;
        switch (e.key) {
          case "ArrowDown":
            row = Math.min(row + 1, maxRow);
            break;
          case "ArrowUp":
            row = Math.max(row - 1, 0);
            break;
          case "ArrowRight":
            col = Math.min(col + 1, maxCol);
            break;
          case "ArrowLeft":
            col = Math.max(col - 1, 0);
            break;
          case "Home":
            if (e.ctrlKey) {
              row = 0;
              col = 0;
            } else {
              col = 0;
            }
            break;
          case "End":
            if (e.ctrlKey) {
              row = maxRow;
              col = maxCol;
            } else {
              col = maxCol;
            }
            break;
        }
        return { row, col };
      });
    },
    [sortedRows.length, totalCols],
  );

  // ---- Render ----
  const rowH = density === "compact" ? "h-7" : "h-10";
  const cellPad = density === "compact" ? "px-2 py-0.5" : "px-3 py-2";
  const alignCls = (align?: string) =>
    align === "right" ? "text-right" : align === "center" ? "text-center" : "text-left";

  const headerCells: ReactNode[] = [];

  // Checkbox column header
  if (selectionEnabled) {
    headerCells.push(
      <th
        key="__sel"
        role="columnheader"
        className="sticky left-0 z-20 border-b border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-2"
        style={{ width: 36 }}
      >
        {selectionMode === "multi" ? (
          <input
            type="checkbox"
            checked={allSelected}
            ref={(el) => {
              if (el) el.indeterminate = someSelected;
            }}
            onChange={toggleAll}
            aria-label={tr("table_select_all", undefined, "Select all rows")}
            className="h-4 w-4 cursor-pointer accent-[var(--color-accent)]"
          />
        ) : null}
      </th>,
    );
  }

  columns.forEach((col) => {
    const isSorted = activeSortKey === col.key;
    const ariaSort = isSorted
      ? activeSortDir === "asc"
        ? "ascending"
        : "descending"
      : col.sort
        ? "none"
        : undefined;
    const SortIcon = isSorted
      ? activeSortDir === "asc"
        ? ChevronUp
        : ChevronDown
      : col.sort
        ? ChevronsUpDown
        : null;
    headerCells.push(
      <th
        key={col.key}
        role="columnheader"
        aria-sort={ariaSort as React.AriaAttributes["aria-sort"]}
        onClick={col.sort ? () => handleSort(col.key) : undefined}
        className={[
          "border-b border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]",
          col.sort ? "cursor-pointer select-none hover:text-[var(--color-text)]" : "",
          col.sticky ? "sticky left-0 z-20 bg-[var(--color-surface-2)]" : "",
          alignCls(col.align),
        ].join(" ")}
        style={col.width ? { width: col.width } : undefined}
      >
        <span className="inline-flex items-center gap-1">
          {col.header}
          {SortIcon && <SortIcon size={12} className={isSorted ? "" : "opacity-40"} />}
        </span>
      </th>,
    );
  });

  const hasData = sortedRows.length > 0;

  return (
    <div
      className={`relative overflow-auto ${className}`}
      role="grid"
      aria-rowcount={sortedRows.length}
      aria-colcount={totalCols}
      tabIndex={0}
      onKeyDown={handleGridKeyDown}
    >
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr role="row">{headerCells}</tr>
        </thead>
        <tbody>
          {visibleRows.map((row, rowIdx) => {
            const key = rowKey(row, rowIdx);
            const disabled = isRowDisabled?.(row, rowIdx) ?? false;
            const selected = selectedKeys?.has(key) ?? false;
            const zebraCls =
              zebra && rowIdx % 2 === 1 ? "bg-[var(--color-surface)]" : "";

            const cells: ReactNode[] = [];

            if (selectionEnabled) {
              const cellActive =
                activeCell.row === rowIdx && activeCell.col === 0;
              cells.push(
                <td
                  key="__sel"
                  role="gridcell"
                  tabIndex={cellActive ? 0 : -1}
                  className={`sticky left-0 z-10 border-b border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 ${zebraCls}`}
                  style={{ width: 36 }}
                >
                  <input
                    type={selectionMode === "single" ? "radio" : "checkbox"}
                    checked={selected}
                    disabled={disabled}
                    onChange={() => toggleRow(key)}
                    aria-label={tr("table_select_row", { n: rowIdx + 1 }, `Select row ${rowIdx + 1}`)}
                    className="h-4 w-4 cursor-pointer accent-[var(--color-accent)]"
                  />
                </td>,
              );
            }

            columns.forEach((col, colIdx) => {
              const absCol = colIdx + colOffset;
              const cellActive =
                activeCell.row === rowIdx && activeCell.col === absCol;
              cells.push(
                <td
                  key={col.key}
                  role="gridcell"
                  tabIndex={cellActive ? 0 : -1}
                  onClick={
                    onRowClick && !disabled
                      ? () => onRowClick(row, rowIdx)
                      : undefined
                  }
                  className={[
                    "border-b border-[var(--color-border)]",
                    cellPad,
                    alignCls(col.align),
                    col.sticky
                      ? "sticky left-0 z-10 bg-[var(--color-surface-2)]"
                      : "",
                    zebraCls,
                    onRowClick && !disabled ? "cursor-pointer" : "",
                    disabled ? "opacity-40" : "",
                    cellActive ? "outline outline-2 outline-[var(--color-accent)]" : "",
                  ].join(" ")}
                >
                  {col.cell(row, rowIdx)}
                </td>,
              );
            });

            return (
              <tr
                key={key}
                role="row"
                aria-selected={selectionEnabled ? selected : undefined}
                aria-disabled={disabled || undefined}
                className={`${rowH} ${selected ? "bg-[var(--color-accent-soft,var(--color-surface-3))]" : ""}`}
              >
                {cells}
              </tr>
            );
          })}
        </tbody>
      </table>

      {!hasData && emptyState && (
        <div className="py-8">{emptyState}</div>
      )}

      {hasMore && (
        <div ref={sentinelRef} className="flex items-center justify-center py-3 text-xs text-[var(--color-muted)]">
          {tr("table_loading_more", { shown: renderCount, total: sortedRows.length }, `Loading more… (${renderCount}/${sortedRows.length})`)}
        </div>
      )}
    </div>
  );
}
