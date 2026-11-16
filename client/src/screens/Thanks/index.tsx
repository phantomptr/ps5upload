// Thanks & Support (/thanks, in the sidebar under Bug Report): a thank-you to everyone who
// supported PS5Upload, and the ways to support it (Ko-fi, or crypto straight to the owner's
// wallet). The supporters list comes from scripts/supporters.mjs, run on the owner's computer
// over a Ko-fi export: only the names are kept, most first (no amounts, emails or messages).

import { writeClipboard } from "../../lib/clipboard";
import { useMemo, useState } from "react";
import qrcode from "qrcode-generator";
import { Check, Coffee, Copy, Heart, MessageCircle, ShieldAlert, Sparkles, Wallet } from "lucide-react";

import { SupportButton } from "../../components/SupportButton";
import { CRYPTO, type CryptoAddress } from "../../data/donate";
import supporters from "../../data/supporters.json";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useTr } from "../../state/lang";

export interface SupporterList {
  /** Most first (scripts/supporters.mjs keeps only the order). */
  names: string[];
  anonymous: number;
}

const DISCORD = "https://discord.gg/fzK3xddtrM";
/** Brand names, the same in every language. */
const KOFI = "Ko-fi";

export default function ThanksScreen({ list = supporters as SupporterList }: { list?: SupporterList }) {
  const tr = useTr();
  const named = list.names.length;
  return (
    <div className="app-page mx-auto max-w-5xl space-y-8 pb-10" data-testid="thanks">
      {/* Hero: the thank-you first, the counts (people, never money). */}
      <header className="relative overflow-hidden rounded-[var(--radius-panel)] border border-[var(--glass-edge)] px-6 py-10 text-center shadow-[var(--edge-highlight),var(--shadow-1)] sm:px-10">
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0"
          style={{
            background:
              "radial-gradient(60% 80% at 50% 0%, color-mix(in srgb, #ff3d6e 22%, transparent), transparent 70%), radial-gradient(40% 60% at 90% 100%, color-mix(in srgb, #ff9a3c 16%, transparent), transparent 70%), var(--color-surface-raised)",
          }}
        />
        <div className="relative flex flex-col items-center gap-4">
          <span className="flex h-14 w-14 items-center justify-center rounded-full text-white shadow-[0_10px_30px_-8px_rgb(255_61_110/0.7)] [background-image:linear-gradient(135deg,#ff3d6e,#ff9a3c)]">
            <Heart size={26} fill="currentColor" aria-hidden className="support-heart" />
          </span>
          <h1 className="text-3xl font-semibold tracking-tight sm:text-4xl">{tr("thanks_title", undefined, "Thank you")}</h1>
          <p className="max-w-2xl text-sm leading-relaxed text-[var(--color-muted)] sm:text-base">
            {tr(
              "thanks_intro",
              undefined,
              "PS5Upload is free, and it keeps getting better because of the people below. Every coffee pays for test hardware, late nights and the next release. Thank you, all of you.",
            )}
          </p>
          <div className="flex flex-wrap justify-center gap-2 pt-1" data-testid="thanks-counts">
            <Stat icon={<Heart size={13} fill="currentColor" />} text={tr("thanks_count_named", { n: named }, `${named} supporters`)} />
            {list.anonymous > 0 && (
              <Stat icon={<Sparkles size={13} />} text={tr("thanks_count_anonymous", { n: list.anonymous }, `+${list.anonymous} anonymous`)} />
            )}
          </div>
        </div>
      </header>

      {/* Ways to support: Ko-fi, or crypto to the wallet. */}
      <section aria-labelledby="thanks-support" className="space-y-3">
        <SectionTitle id="thanks-support" icon={<Wallet size={16} />}>
          {tr("thanks_support_title", undefined, "Support PS5Upload")}
        </SectionTitle>
        <div className="grid gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
          <KofiCard />
          <CryptoCard />
        </div>
      </section>

      {/* The wall: everyone, most first, one list. */}
      <section aria-labelledby="thanks-wall" className="space-y-4">
        <SectionTitle id="thanks-wall" icon={<Heart size={16} />}>
          {tr("thanks_wall_title", undefined, "Wall of thanks")}
        </SectionTitle>
        <ul className="flex flex-wrap justify-center gap-2" data-testid="thanks-names">
          {list.names.map((name) => (
            <li
              key={name}
              className="rounded-full border border-[color-mix(in_srgb,#ff3d6e_35%,transparent)] bg-[color-mix(in_srgb,#ff3d6e_8%,transparent)] px-3.5 py-1.5 text-sm"
            >
              {name}
            </li>
          ))}
        </ul>
        {list.anonymous > 0 && (
          <p className="pt-2 text-center text-sm text-[var(--color-muted)]" data-testid="thanks-anonymous">
            {tr(
              "thanks_anonymous",
              { n: list.anonymous },
              `…and ${list.anonymous} more who chose to stay anonymous. Thank you too.`,
            )}
          </p>
        )}
      </section>

      <footer className="flex flex-col items-center gap-3 border-t border-[var(--color-border)] pt-6 text-center">
        <p className="max-w-xl text-xs text-[var(--color-muted)]">
          {tr(
            "thanks_listing_note",
            undefined,
            "Names are shown as you gave them on Ko-fi, and the list grows with each release. Want yours changed or taken off? Just say so on Discord.",
          )}
        </p>
        <button
          type="button"
          onClick={() => void openExternalUrl(DISCORD)}
          className="inline-flex items-center gap-1.5 rounded-full border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-raised)]"
        >
          <MessageCircle size={13} aria-hidden />
          Discord
        </button>
      </footer>
    </div>
  );
}

function SectionTitle({ id, icon, children }: { id: string; icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <h2 id={id} className="flex items-center gap-2 text-base font-semibold">
      <span className="text-[var(--color-muted)]">{icon}</span>
      {children}
    </h2>
  );
}

function Stat({ icon, text }: { icon: React.ReactNode; text: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1 text-xs font-medium">
      <span className="text-[#ff3d6e]">{icon}</span>
      {text}
    </span>
  );
}

function Card({ children, className = "", testId }: { children: React.ReactNode; className?: string; testId?: string }) {
  return (
    <div
      data-testid={testId}
      className={`rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-5 shadow-[var(--edge-highlight),var(--shadow-1)] ${className}`}
    >
      {children}
    </div>
  );
}

function KofiCard() {
  const tr = useTr();
  return (
    <Card className="flex flex-col gap-4" testId="thanks-kofi">
      <div className="flex items-center gap-3">
        <span className="flex h-10 w-10 items-center justify-center rounded-xl bg-[color-mix(in_srgb,#ff5e5b_16%,transparent)] text-[#ff5e5b]">
          <Coffee size={20} aria-hidden />
        </span>
        <div>
          <h3 className="text-sm font-semibold">{KOFI}</h3>
          <p className="text-xs text-[var(--color-muted)]">{tr("thanks_kofi_sub", undefined, "Card or PayPal, once or monthly")}</p>
        </div>
      </div>
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "thanks_kofi_body",
          undefined,
          "The easiest way: a coffee, or whatever you like. Your name joins the wall with the next release (or stay anonymous).",
        )}
      </p>
      <div className="mt-auto">
        <SupportButton className="min-h-10 px-5 text-sm" />
      </div>
    </Card>
  );
}

function CryptoCard() {
  const tr = useTr();
  const [pick, setPick] = useState<CryptoAddress["id"]>("btc");
  const c = CRYPTO.find((x) => x.id === pick) ?? CRYPTO[0];
  return (
    <Card className="flex flex-col gap-4" testId="thanks-crypto">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold">{tr("thanks_crypto_title", undefined, "Crypto")}</h3>
          <p className="text-xs text-[var(--color-muted)]">{tr("thanks_crypto_sub", undefined, "Straight to my hardware wallet")}</p>
        </div>
        <div role="tablist" aria-label={tr("thanks_crypto_title", undefined, "Crypto")} className="flex gap-1 rounded-full border border-[var(--color-border)] bg-[var(--color-surface)] p-1">
          {CRYPTO.map((x) => {
            const on = x.id === pick;
            return (
              <button
                key={x.id}
                type="button"
                role="tab"
                aria-selected={on}
                data-testid={`thanks-coin-${x.id}`}
                onClick={() => setPick(x.id)}
                className="rounded-full px-3 py-1 text-xs font-semibold transition-colors"
                style={
                  on
                    ? { background: x.color, color: "#fff", boxShadow: `0 4px 14px -6px ${x.color}` }
                    : { color: "var(--color-muted)" }
                }
              >
                {x.coin}
              </button>
            );
          })}
        </div>
      </div>
      <div className="flex flex-col items-center gap-4 sm:flex-row sm:items-start" role="tabpanel">
        <div className="shrink-0 rounded-2xl p-1.5" style={{ background: `linear-gradient(135deg, ${c.color}, color-mix(in srgb, ${c.color} 40%, transparent))` }}>
          <Qr text={c.address} label={`${c.name} ${tr("thanks_qr", undefined, "QR code")}`} />
        </div>
        <div className="min-w-0 flex-1 space-y-3">
          <div>
            <p className="text-sm font-semibold" style={{ color: c.color }}>
              {c.name}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr("thanks_network", { network: c.network }, `Network: ${c.network}`)}
            </p>
          </div>
          <Address value={c.address} />
          <p className="flex items-start gap-1.5 text-xs text-[var(--color-warn)]" data-testid="thanks-network-warning">
            <ShieldAlert size={14} className="mt-px shrink-0" aria-hidden />
            {tr(
              "thanks_network_only",
              { coin: c.coin, network: c.network },
              `Send only ${c.coin} on the ${c.network} network. Anything else can't be recovered.`,
            )}
          </p>
          {c.id === "xrp" && (
            <p className="text-xs text-[var(--color-muted)]">
              {tr("thanks_xrp_no_tag", undefined, "No destination tag needed: this is my own wallet, not an exchange.")}
            </p>
          )}
        </div>
      </div>
    </Card>
  );
}

/** The address in groups of four (easier to check by eye); Copy takes it exactly. */
function Address({ value }: { value: string }) {
  const tr = useTr();
  const [copied, setCopied] = useState(false);
  const groups = value.match(/.{1,4}/g) ?? [value];
  const copy = async () => {
    // writeClipboard also works on plain http, where the browser has no navigator.clipboard.
    if (await writeClipboard(value)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  };
  return (
    <div className="flex items-stretch gap-2">
      <code
        className="min-w-0 flex-1 select-all break-all rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 font-mono text-[13px] leading-relaxed"
        data-testid="thanks-address"
        aria-label={value}
      >
        {groups.map((g, i) => (
          <span key={i} className={i % 2 ? "text-[var(--color-muted)]" : undefined}>
            {g}
          </span>
        ))}
      </code>
      <button
        type="button"
        onClick={() => void copy()}
        data-testid="thanks-copy"
        className="inline-flex shrink-0 items-center gap-1.5 rounded-lg border border-[var(--color-border)] px-3 text-xs font-medium hover:bg-[var(--color-surface)]"
      >
        {copied ? <Check size={14} className="text-[var(--color-good)]" aria-hidden /> : <Copy size={14} aria-hidden />}
        {copied ? tr("thanks_copied", undefined, "Copied") : tr("thanks_copy", undefined, "Copy")}
      </button>
    </div>
  );
}

/** A QR code, dark on white (wallet cameras read it in either theme). */
function Qr({ text, label }: { text: string; label: string }) {
  const { path, n } = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(text);
    qr.make();
    const count = qr.getModuleCount();
    let d = "";
    for (let y = 0; y < count; y++) for (let x = 0; x < count; x++) if (qr.isDark(y, x)) d += `M${x + 3} ${y + 3}h1v1h-1z`;
    return { path: d, n: count + 6 };
  }, [text]);
  return (
    <svg viewBox={`0 0 ${n} ${n}`} role="img" aria-label={label} className="block h-36 w-36 rounded-xl bg-white" shapeRendering="crispEdges">
      <path d={path} fill="#111" />
    </svg>
  );
}
