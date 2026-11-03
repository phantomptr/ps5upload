// "Coffee or tokens": the one button in the app that asks for support. Warm and a little alive
// (a heart that beats now and then) so it reads as a thank-you, not one more setting; still for
// anyone who turned motion down.

import { Heart } from "lucide-react";
import { useNavigate } from "react-router";

import { COFFEE_URL } from "../lib/supportLinks";
import { openExternalUrl } from "../lib/openExternalUrl";
import { useTr } from "../state/lang";

export function SupportButton({ className = "" }: { className?: string }) {
  const tr = useTr();
  return (
    <button
      type="button"
      onClick={() => void openExternalUrl(COFFEE_URL)}
      className={`support-button inline-flex min-h-9 items-center gap-1.5 whitespace-nowrap rounded-full px-4 text-xs font-semibold ${className}`}
    >
      <Heart size={13} aria-hidden fill="currentColor" className="support-heart shrink-0" />
      {tr("buy_coffee", undefined, "Coffee or tokens")}
    </button>
  );
}

/** The same warm button, to Thanks & Support (where Ko-fi and crypto live): on Home. */
export function ThanksButton({ className = "" }: { className?: string }) {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <button
      type="button"
      onClick={() => navigate("/thanks")}
      className={`support-button inline-flex min-h-9 items-center gap-1.5 whitespace-nowrap rounded-full px-4 text-xs font-semibold ${className}`}
    >
      <Heart size={13} aria-hidden fill="currentColor" className="support-heart shrink-0" />
      {tr("thanks_tab", undefined, "Thank you")}
    </button>
  );
}
