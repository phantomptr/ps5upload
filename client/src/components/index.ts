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
export type { ButtonProps } from "./Button";
export { MarkdownView } from "./MarkdownView";
export type {
  ConfirmOptions,
  AlertOptions,
  PromptOptions,
} from "./ConfirmDialog";
export { OverflowMenu, type OverflowMenuItem } from "./OverflowMenu";
export { RootErrorBoundary } from "./ErrorBoundary";
