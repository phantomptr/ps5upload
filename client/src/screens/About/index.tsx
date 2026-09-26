import { useEffect, useState } from "react";
import {
  Heart,
  Coffee,
  Mail,
  Zap,
  HardDrive,
  Radio,
  Cpu,
  Sparkles,
  ShieldAlert,
} from "lucide-react";
import { openExternalUrl as openExternal } from "../../lib/openExternalUrl";
import { getAppVersion } from "../../lib/appVersion";

import { useTr } from "../../state/lang";
import { Button, Card, Badge } from "../../components";

const AUTHOR_EMAIL = "phantomptr@gmail.com";

const URLS = {
  github: "https://github.com/phantomptr/ps5upload",
  issues: "https://github.com/phantomptr/ps5upload/issues",
  license: "https://github.com/phantomptr/ps5upload/blob/main/LICENSE",
  changelog: "https://github.com/phantomptr/ps5upload/blob/main/CHANGELOG.md",
  disclaimer: "https://github.com/phantomptr/ps5upload/blob/main/DISCLAIMER.md",
  author: "https://x.com/phantomptr",
  discord: "https://discord.gg/fzK3xddtrM",
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
    bodyFallback:
      "FTX2 binary protocol with BLAKE3 shard verification + pack-small-files optimization. Uses your LAN flat-out.",
  },
  {
    icon: HardDrive,
    titleKey: "about_feat_native_mount_title",
    titleFallback: "Native image mount",
    bodyKey: "about_feat_native_mount_body",
    bodyFallback:
      "Attach .exfat and .ffpkg images to /mnt/ps5upload/ via MDIOCATTACH + nmount — no third-party helpers needed.",
  },
  {
    icon: Radio,
    titleKey: "about_feat_works_everything_title",
    titleFallback: "Works with everything",
    bodyKey: "about_feat_works_everything_body",
    bodyFallback:
      "Send any PS5 payload ELF — homebrew loaders, kernel patches, custom utilities — over :9021 with a file-picker flow.",
  },
  {
    icon: Cpu,
    titleKey: "about_feat_hardware_title",
    titleFallback: "Live hardware view",
    bodyKey: "about_feat_hardware_body",
    bodyFallback:
      "Model, serial, uptime, CPU frequency, RAM, and fan-threshold control — all without touching Sony's UI.",
  },
];

export default function AboutScreen() {
  const tr = useTr();
  const [version, setVersion] = useState<string>("");

  useEffect(() => {
    getAppVersion()
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
          className="mb-5 h-24 w-24 rounded-2xl shadow-[var(--shadow-logo)]"
        />
        <h1 className="text-4xl font-semibold tracking-tight">PS5Upload</h1>
        <div className="mt-2 flex items-center gap-2">
          {version && (
            <Badge tone="neutral" variant="outline" size="md" className="font-mono">
              v{version}
            </Badge>
          )}
          <Badge tone="good" variant="soft" size="md">GPL-3</Badge>
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
            icon={<GithubIcon />}
            label="GitHub"
            onClick={() => openExternal(URLS.github)}
          />
          <PrimaryLink
            icon={<XIcon />}
            label="@phantomptr"
            onClick={() => openExternal(URLS.author)}
          />
          <PrimaryLink
            icon={<DiscordIcon />}
            label="Discord"
            onClick={() => openExternal(URLS.discord)}
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
        <SectionTitle icon={Sparkles}>
          {tr("about_what_it_does", undefined, "What it does")}
        </SectionTitle>
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
              <div className="text-xs uppercase tracking-wider text-[var(--color-muted)]">
                {tr("created_by", undefined, "Created by")}
              </div>
              <button
                type="button"
                onClick={() => openExternal(URLS.author)}
                className="mt-0.5 text-base font-medium hover:text-[var(--color-accent)] max-md:inline-flex max-md:min-h-11 max-md:items-center"
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
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Mail size={12} />}
              onClick={() => openExternal(URLS.email)}
              className="shrink-0"
            >
              {AUTHOR_EMAIL}
            </Button>
          </div>
        </Card>
      </section>

      {/* Research-purpose notice. This tool drives a jailbroken console;
          the risks are real and the user carries them, so state that
          plainly in the app rather than only in the repo. Full terms
          live in DISCLAIMER.md. */}
      <section className="mb-8">
        <SectionTitle icon={ShieldAlert}>
          {tr("about_disclaimer_title", undefined, "Disclaimer")}
        </SectionTitle>
        <Card className="p-4">
          <p className="text-sm leading-relaxed text-[var(--color-muted)]">
            {tr(
              "about_disclaimer_purpose",
              undefined,
              "ps5upload is provided for research, educational, interoperability, and homebrew development purposes only. It is not affiliated with, endorsed by, or connected to Sony Interactive Entertainment Inc. \u201cPlayStation\u201d and \u201cPS5\u201d are trademarks of Sony Interactive Entertainment Inc., used here for identification only.",
            )}
          </p>
          <p className="mt-3 text-sm leading-relaxed text-[var(--color-muted)]">
            {tr(
              "about_disclaimer_risk",
              undefined,
              "This software is provided \u201cas is\u201d, without warranty of any kind. Operating a modified console can permanently damage it, irreversibly destroy saves and licences, and get your account or console banned. You use it entirely at your own risk and accept full responsibility for the consequences. Back up anything you cannot afford to lose before you begin.",
            )}
          </p>
          <p className="mt-3 text-sm leading-relaxed text-[var(--color-muted)]">
            {tr(
              "about_disclaimer_piracy",
              undefined,
              "Use it only with hardware you own and content you lawfully own. It must not be used to copy, distribute, or run software you do not have the right to use.",
            )}
          </p>
          <div className="mt-4">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<ShieldAlert size={12} />}
              onClick={() => openExternal(URLS.disclaimer)}
            >
              {tr("about_disclaimer_read_full", undefined, "Read the full terms")}
            </Button>
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
          {tr("about_made_with", "Made with")}{" "}
          <Heart
            size={10}
            className="mx-0.5 inline -translate-y-[1px] text-[var(--color-bad)]"
          />{" "}
          {tr("about_for_the_ps5_scene", "for the PS5 scene.")}
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
    <h2 className="mb-3 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
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

/** Hero links are the shared Button primitive (md so they read as
 *  CTAs); `accent` maps to the primary variant for the coffee link. */
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
    <Button
      variant={accent ? "primary" : "secondary"}
      size="md"
      leftIcon={icon}
      onClick={onClick}
    >
      {label}
    </Button>
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
      className="hover:text-[var(--color-text)] hover:underline max-md:inline-flex max-md:min-h-11 max-md:items-center"
    >
      {label}
    </button>
  );
}

/** Lucide doesn't ship an X-the-social-network icon; this is an
 *  inline SVG at Lucide's stroke-width so it fits visually. */
function DiscordIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={14}
      height={14}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden
    >
      <path d="M20.317 4.369a19.791 19.791 0 0 0-4.885-1.515.074.074 0 0 0-.079.037c-.211.375-.445.865-.608 1.25a18.27 18.27 0 0 0-5.487 0 12.64 12.64 0 0 0-.617-1.25.077.077 0 0 0-.079-.037A19.736 19.736 0 0 0 3.677 4.37a.07.07 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 0 0 .031.057 19.9 19.9 0 0 0 5.993 3.03.078.078 0 0 0 .084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 0 0-.041-.106 13.107 13.107 0 0 1-1.872-.892.077.077 0 0 1-.008-.128c.126-.094.252-.192.372-.291a.074.074 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 0 1 .078.009c.12.099.246.198.373.292a.077.077 0 0 1-.006.127 12.3 12.3 0 0 1-1.873.892.077.077 0 0 0-.041.107c.36.698.772 1.363 1.225 1.993a.076.076 0 0 0 .084.028 19.839 19.839 0 0 0 6.002-3.03.077.077 0 0 0 .032-.056c.5-5.177-.838-9.674-3.549-13.66a.06.06 0 0 0-.031-.028ZM8.02 15.331c-1.183 0-2.157-1.086-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.332-.956 2.418-2.157 2.418Zm7.975 0c-1.183 0-2.157-1.086-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.332-.946 2.418-2.157 2.418Z" />
    </svg>
  );
}

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

// lucide-react v1 removed brand icons (Github, etc.), so we inline the
// mark — same pattern as XIcon above.
function GithubIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={14}
      height={14}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden
    >
      <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23A11.509 11.509 0 0 1 12 5.803c1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222 0 1.606-.014 2.898-.014 3.293 0 .322.216.694.825.576C20.565 22.092 24 17.595 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  );
}
