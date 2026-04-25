import { useEffect, useState } from "react";
import {
  Github,
  Heart,
  Coffee,
  Mail,
  Zap,
  HardDrive,
  Radio,
  Cpu,
  Sparkles,
} from "lucide-react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { getVersion } from "@tauri-apps/api/app";

import { useTr } from "../../state/lang";
import { Card } from "../../components";

const AUTHOR_EMAIL = "phantomptr@gmail.com";

const URLS = {
  github: "https://github.com/phantomptr/ps5upload",
  issues: "https://github.com/phantomptr/ps5upload/issues",
  license: "https://github.com/phantomptr/ps5upload/blob/main/LICENSE",
  changelog: "https://github.com/phantomptr/ps5upload/blob/main/CHANGELOG.md",
  author: "https://x.com/phantomptr",
  coffee: "https://ko-fi.com/B0B81S0WUA",
  email: `mailto:${AUTHOR_EMAIL}`,
};

/** Top-level feature cards — four one-sentence pitches of what the app
 *  actually does. Helps first-time About-page visitors quickly scan
 *  "is this what I need?". Icons match the feature's sidebar icon so
 *  there's visual continuity if the user jumps over to try it. */
/** Module-level constants can't reach `useTr` (it's a hook). Each
 *  feature carries a {key, fallback} pair instead; the rendering
 *  loop calls `tr()` per feature so language changes flow through. */
const FEATURES: {
  icon: typeof Zap;
  titleKey: string;
  titleFallback: string;
  bodyKey: string;
  bodyFallback: string;
}[] = [
  {
    icon: Zap,
    titleKey: "about_feat_fast_transfers_title",
    titleFallback: "Fast transfers",
    bodyKey: "about_feat_fast_transfers_body",
    bodyFallback: "FTX2 binary protocol with BLAKE3 shard verification + pack-small-files optimization. Uses your LAN flat-out.",
  },
  {
    icon: HardDrive,
    titleKey: "about_feat_native_mount_title",
    titleFallback: "Native image mount",
    bodyKey: "about_feat_native_mount_body",
    bodyFallback: "Attach .exfat and .ffpkg images to /mnt/ps5upload/ via MDIOCATTACH + nmount — no third-party helpers needed.",
  },
  {
    icon: Radio,
    titleKey: "about_feat_works_everything_title",
    titleFallback: "Works with everything",
    bodyKey: "about_feat_works_everything_body",
    bodyFallback: "Send any PS5 payload ELF — homebrew loaders, kernel patches, custom utilities — over :9021 with a file-picker flow.",
  },
  {
    icon: Cpu,
    titleKey: "about_feat_hardware_title",
    titleFallback: "Live hardware view",
    bodyKey: "about_feat_hardware_body",
    bodyFallback: "Model, serial, uptime, CPU frequency, RAM, and fan-threshold control — all without touching Sony's UI.",
  },
];

export default function AboutScreen() {
  const tr = useTr();
  const [version, setVersion] = useState<string>("");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <div className="mx-auto max-w-5xl p-6">
      {/* Hero — large logo, big type, version pill, one-line pitch,
          three primary links. All centered on narrow screens; the
          desktop two-column treatment lives below. */}
      <header className="flex flex-col items-center pt-4 pb-10 text-center">
        <img
          src="/logo-square.png"
          alt=""
          aria-hidden
          className="mb-5 h-24 w-24 rounded-2xl shadow-[0_12px_40px_-10px_rgba(0,0,0,0.5)]"
        />
        <h1 className="text-4xl font-semibold tracking-tight">PS5Upload</h1>
        <div className="mt-2 flex items-center gap-2">
          {version && (
            <span className="rounded-full border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2.5 py-0.5 font-mono text-xs text-[var(--color-muted)]">
              v{version}
            </span>
          )}
          <span className="rounded-full border border-[var(--color-good)] bg-[var(--color-good-soft)] px-2.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-good)]">
            GPL-3
          </span>
        </div>
        <p className="mt-5 max-w-xl text-base leading-relaxed text-[var(--color-muted)]">
          {tr(
            "about_tagline",
            undefined,
            "Fast, reliable uploads from your computer to your PS5. Transfer, mount, and browse — designed to live alongside your PS5-side tools.",
          )}
        </p>

        <div className="mt-6 flex flex-wrap items-center justify-center gap-2">
          <PrimaryLink
            icon={<Github size={14} />}
            label="GitHub"
            onClick={() => openExternal(URLS.github)}
          />
          <PrimaryLink
            icon={<XIcon />}
            label="@phantomptr"
            onClick={() => openExternal(URLS.author)}
          />
          <PrimaryLink
            icon={<Coffee size={14} />}
            label={tr("buy_coffee")}
            accent
            onClick={() => openExternal(URLS.coffee)}
          />
        </div>
      </header>

      {/* Features — 2×2 grid on sm+, 4-wide on lg+. Compact icon
          cards so the page doesn't turn into a wall of words. */}
      <section className="mb-10">
        <SectionTitle icon={Sparkles}>{tr("about_what_it_does", undefined, "What it does")}</SectionTitle>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          {FEATURES.map((f) => (
            <FeatureTile
              key={f.titleKey}
              icon={f.icon}
              title={tr(f.titleKey, undefined, f.titleFallback)}
              body={tr(f.bodyKey, undefined, f.bodyFallback)}
            />
          ))}
        </div>
      </section>

      {/* Author — single row, modest weight. */}
      <section className="mb-8">
        <SectionTitle>{tr("about_credits", undefined, "Credits")}</SectionTitle>
        <Card>
          <div className="flex flex-col gap-3 text-sm md:flex-row md:items-center md:justify-between">
            <div>
              <div className="text-[11px] uppercase tracking-wider text-[var(--color-muted)]">
                {tr("created_by", undefined, "Created by")}
              </div>
              <button
                type="button"
                onClick={() => openExternal(URLS.author)}
                className="mt-0.5 text-base font-medium hover:text-[var(--color-accent)]"
              >
                PhantomPtr
              </button>
              <p className="mt-2 max-w-lg text-xs leading-relaxed text-[var(--color-muted)]">
                {tr(
                  "about_credits_text",
                  undefined,
                  "See LICENSE and the project README for credits to the upstream projects ps5upload builds on.",
                )}
              </p>
            </div>
            <button
              type="button"
              onClick={() => openExternal(URLS.email)}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-[var(--color-border)] px-3 py-1.5 text-xs text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            >
              <Mail size={12} />
              {AUTHOR_EMAIL}
            </button>
          </div>
        </Card>
      </section>

      {/* Footer links + made-with */}
      <footer className="flex flex-col items-center gap-3 pb-4 text-xs text-[var(--color-muted)]">
        <div className="flex flex-wrap items-center justify-center gap-4">
          <FooterLink
            label={tr("report_issue", undefined, "Report an issue")}
            onClick={() => openExternal(URLS.issues)}
          />
          <span className="opacity-30">·</span>
          <FooterLink
            label={tr("license", undefined, "License")}
            onClick={() => openExternal(URLS.license)}
          />
          <span className="opacity-30">·</span>
          <FooterLink
            label={tr("changelog", undefined, "Changelog on GitHub")}
            onClick={() => openExternal(URLS.changelog)}
          />
        </div>
        <p className="opacity-60">
          Made with{" "}
          <Heart
            size={10}
            className="mx-0.5 inline -translate-y-[1px] text-[var(--color-bad)]"
          />{" "}
          for the PS5 scene.
        </p>
      </footer>
    </div>
  );
}

function SectionTitle({
  icon: Icon,
  children,
}: {
  icon?: typeof Sparkles;
  children: React.ReactNode;
}) {
  return (
    <h2 className="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wider text-[var(--color-muted)]">
      {Icon && <Icon size={13} />}
      <span>{children}</span>
    </h2>
  );
}

function FeatureTile({
  icon: Icon,
  title,
  body,
}: {
  icon: typeof Zap;
  title: string;
  body: string;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <Icon size={18} className="text-[var(--color-accent)]" />
      <div className="text-sm font-semibold">{title}</div>
      <div className="text-xs leading-relaxed text-[var(--color-muted)]">
        {body}
      </div>
    </div>
  );
}


function PrimaryLink({
  icon,
  label,
  accent,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  accent?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={
        "inline-flex items-center gap-2 rounded-lg border px-3.5 py-1.5 text-sm transition-colors " +
        (accent
          ? "border-[var(--color-accent)] bg-[var(--color-accent)] text-[var(--color-accent-contrast)] hover:opacity-90"
          : "border-[var(--color-border)] bg-[var(--color-surface-2)] hover:bg-[var(--color-surface-3)]")
      }
    >
      {icon}
      {label}
    </button>
  );
}

function FooterLink({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="hover:text-[var(--color-text)] hover:underline"
    >
      {label}
    </button>
  );
}

/** Lucide doesn't ship an X-the-social-network icon; this is an
 *  inline SVG at Lucide's stroke-width so it fits visually. */
function XIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={13}
      height={13}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden
    >
      <path d="M18.244 2h3.308l-7.227 8.26L22.5 22h-6.828l-5.348-6.996L4.02 22H.71l7.73-8.835L.5 2h7.003l4.835 6.392L18.244 2Zm-1.161 18.08h1.834L7.02 3.826H5.052l12.03 16.254Z" />
    </svg>
  );
}
