import { Send } from "lucide-react";

import { Button } from "./Button";
import { useConnectionStore } from "../state/connection";
import { helperSendFor, useHelperSendStore } from "../state/helperSend";
import { sendHelperTo } from "../state/helperSendRuntime";
import { useTr } from "../state/lang";
import { useHelperReplaceGuard } from "../screens/Connection/useHelperReplaceGuard";

/** Sends the helper to the selected console, with the send's step or failure under the button.
 *  Offered wherever the helper being down stops the user, so the fix is where the problem is
 *  instead of on the Connection screen. Renders nothing without a console address or an
 *  engine to send through. */
export function SendHelperButton({
  size,
  wide,
}: {
  size?: "sm" | "md";
  wide?: boolean;
}) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const engineUp = useConnectionStore((s) => s.engineStatus === "up");
  const send = useHelperSendStore((s) => helperSendFor(s, host));
  const { guard, dialog } = useHelperReplaceGuard();
  if (!host.trim() || !engineUp) return null;
  return (
    <div className={wide ? "w-full" : undefined} data-testid="send-helper">
      {dialog}
      <Button
        variant="primary"
        size={size}
        leftIcon={<Send size={14} />}
        loading={send?.state === "busy"}
        onClick={() =>
          void guard(host).then((ok) => {
            if (ok) void sendHelperTo(host, tr);
          })
        }
        className={wide ? "w-full" : undefined}
      >
        {tr("drive_send_helper", undefined, "Send helper")}
      </Button>
      {send && (
        <p
          className={`mt-2 text-xs ${
            send.state === "fail" ? "text-[var(--color-bad)]" : "text-[var(--color-muted)]"
          }`}
        >
          {send.msg}
        </p>
      )}
    </div>
  );
}
