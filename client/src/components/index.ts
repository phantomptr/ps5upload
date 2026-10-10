// Barrel re-export for the shared UI primitives. Importing from
// `../../components` (rather than the individual files) keeps screen
// imports short and makes it obvious at a glance which pieces of the
// design system a given screen uses.
//
// Dialog hooks (useConfirm/useAlert/usePrompt) are deliberately NOT
// re-exported here. Rollup flagged the barrel-then-back imports as a
// circular dep that produced "broken execution order" warnings at
// build. Callers import dialog hooks directly from
// `components/ConfirmDialog` instead.
export { PageHeader } from "./PageHeader";
export { EmptyState } from "./EmptyState";
export { ErrorCard, SuccessCard, WarningCard } from "./ErrorCard";
export { Card } from "./Card";
export { Button } from "./Button";
export { BrowseButton, PathLabel } from "./BrowseButton";
export type { ButtonProps } from "./Button";
export { MarkdownView } from "./MarkdownView";
export type {
  ConfirmOptions,
  AlertOptions,
  PromptOptions,
} from "./ConfirmDialog";
export { OverflowMenu, type OverflowMenuItem } from "./OverflowMenu";
export { Table, type TableColumn, type TableProps, type SortDirection } from "./Table";
export { GameIcon } from "./GameIcon";
export { PlatformBadge } from "./PlatformBadge";
export { Modal } from "./Modal";
export { ConsoleChip } from "./ConsoleChip";
export { RootErrorBoundary } from "./ErrorBoundary";
export { ProgressBar } from "./ProgressBar";
export { Skeleton, SkeletonRows } from "./Skeleton";
export { ShapesLoader } from "./ShapesLoader";
export { ConnectionGate } from "./ConnectionGate";
export { Spinner } from "./Spinner";
export { Input } from "./Input";
export { Select } from "./Select";
export { Textarea } from "./Textarea";
export { Checkbox } from "./Checkbox";
export { Toggle } from "./Toggle";
export { Badge } from "./Badge";
export { Callout } from "./Callout";
export { SegmentedControl } from "./SegmentedControl";
export { Tabs } from "./Tabs";
export { Drawer } from "./Drawer";
export { Toaster } from "./Toaster";
export { SkipNav } from "./SkipNav";
export { LiveRegion } from "./LiveRegion";
export { Sparkline } from "./Sparkline";
