import { useCallback, useRef, useState } from "react";

import { Button, Modal } from "../../components";
import { addConsoleByAddress, addressChoiceNeeded, selectConsoleByAddress } from "../../state/roster";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";

type Choice = "add" | "move" | "cancel";

/**
 * Committing a typed address. An address the roster has not got, on a console the app has seen,
 * may be another PS5 or the same one somewhere new. Re-pointing without asking threw the first
 * console out of the roster and left its wake and Remote Play keys on the second, so the person
 * says which. A console never seen has nothing to lose, so a typo there is just corrected.
 *
 * `commit` resolves false when they cancelled: the address did not change.
 */
export function useConsoleAddressCommit(): {
  commit: (value: string, beforeChange?: () => void) => Promise<boolean>;
  dialog: React.ReactNode;
} {
  const tr = useTr();
  const [asking, setAsking] = useState<{ value: string; answer: (c: Choice) => void } | null>(null);
  // Blur fires as the dialog takes focus: one question per commit.
  const open = useRef(false);
  const commit = useCallback(async (value: string, beforeChange?: () => void) => {
    if (open.current) return false;
    let choice: Choice = "move";
    if (addressChoiceNeeded(value)) {
      open.current = true;
      choice = await new Promise<Choice>((answer) => setAsking({ value, answer }));
      open.current = false;
      setAsking(null);
    }
    if (choice === "cancel") return false;
    beforeChange?.();
    if (choice === "add") addConsoleByAddress(value);
    else selectConsoleByAddress(value);
    return true;
  }, []);
  const current = useConnectionStore((s) => s.host);
  const dialog = (
    <Modal
      open={asking !== null}
      onClose={() => asking?.answer("cancel")}
      title={tr("connection_address_choice_title", undefined, "Is this a different PS5?")}
      size="sm"
      footer={
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={() => asking?.answer("cancel")}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="secondary" size="sm" onClick={() => asking?.answer("move")}>
            {tr("connection_address_choice_move", undefined, "Same PS5, new address")}
          </Button>
          <Button variant="primary" size="sm" onClick={() => asking?.answer("add")}>
            {tr("connection_address_choice_add", undefined, "Add as another PS5")}
          </Button>
        </div>
      }
    >
      <p className="text-sm">
        {tr(
          "connection_address_choice_body",
          { old: current, next: asking?.value ?? "" },
          "{next} is not in your list of consoles. Add it as another PS5, or tell the app the PS5 at {old} has moved there? A move keeps its wake and Remote Play keys.",
        )}
      </p>
    </Modal>
  );
  return { commit, dialog };
}
