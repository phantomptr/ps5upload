import { useEffect, useRef, useState } from "react";
import { Check, Copy } from "lucide-react";

import { Button } from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import { useTr } from "../../state/lang";

/** A command, address or config to copy, always left to right as code is in any language. `plain` is
 *  for words a person would say, which follow their own language's direction. */
export function CopyBlock({
  text,
  label,
  plain = false,
}: {
  text: string;
  label?: string;
  plain?: boolean;
}) {
  const tr = useTr();
  const [copied, setCopied] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const copy = async () => {
    if (!(await writeClipboard(text))) return;
    setCopied(true);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setCopied(false), 1500);
  };
  return (
    <div className="min-w-0">
      {label && (
        <div className="mb-1 text-xs text-[var(--color-muted)]">{label}</div>
      )}
      <div className="flex items-start gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2">
        <pre
          dir={plain ? "auto" : "ltr"}
          className={`min-w-0 flex-1 whitespace-pre-wrap text-xs leading-relaxed [overflow-wrap:anywhere] ${
            plain ? "font-sans" : "text-start font-mono"
          }`}
        >
          {text}
        </pre>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => void copy()}
          aria-label={tr("copy", undefined, "Copy")}
          title={tr("copy", undefined, "Copy")}
          leftIcon={copied ? <Check size={14} /> : <Copy size={14} />}
        />
      </div>
    </div>
  );
}
