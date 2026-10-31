import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, call, isOffline } from "../api";
import { fullscreenState, setFullscreenPref, watchFullscreen } from "../fullscreen";
import { onNavAction } from "../nav";
import { LANGUAGE_NAMES } from "../languages";
import { chooseLanguage, LANGUAGES, storedLanguage, t } from "../strings";
import {
  getContrast,
  getTextSize,
  getTheme,
  setContrast,
  setTextSize,
  setTheme,
  TEXT_SIZES,
  THEMES,
  type TextSize,
  type Theme,
} from "../theme";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { forgetPhoneUrl } from "../ui/PhoneHandoff";

/** /api/settings (payload/src/webui_api.c send_settings). */
export interface WebSettings {
  lan: boolean;
  tile: boolean;
  /** The helper is still adding or removing the tile (Sony calls take seconds). */
  tile_busy?: boolean;
  /** The last add/remove failed: the tile is not as `tile` says. */
  tile_failed?: boolean;
  /** What the helper last did to the tile: "added", "removed", "hidden" (the PS5 kept its record,
   *  so the icon was hidden instead), "failed" or "" (#480; older helpers send none). */
  tile_result?: string;
  /** PS5Upload Minion as a whole (#480); older helpers send none and cannot turn it off. */
  enabled?: boolean;
  /** The page opens on Cheats instead of Home (#584); older helpers send none (Home). */
  start_cheats?: boolean;
  version: string;
}

/** The section the page opens on: Cheats when the helper saved that start page and the page has a
 *  Cheats section, else null (Home). */
export function startSection(s: Pick<WebSettings, "start_cheats"> | null, hasCheats: boolean): "cheats" | null {
  return s?.start_cheats === true && hasCheats ? "cheats" : null;
}

/** Whether this helper can turn Minion off (it sends `enabled`). */
export function canTurnOff(s: WebSettings | null): boolean {
  return typeof s?.enabled === "boolean";
}

/** The note under the tile switch: says when removing the tile could only hide its icon. */
export function tileNote(s: WebSettings): string | undefined {
  if (s.tile || s.tile_busy || s.tile_failed || s.tile_result !== "hidden") return undefined;
  return t("cw_settings_tile_hidden");
}

/** How often, and how long at most, Settings re-reads after a tile switch. */
export const TILE_POLL_MS = 1000;
export const TILE_WAIT_MS = 60_000;

/**
 * After a tile switch: re-read /api/settings until the helper has applied it
 * (tile_busy false) or `maxMs` passed. Resolves to the last answer read.
 */
export async function waitTileApplied(
  first: WebSettings,
  read: () => Promise<WebSettings>,
  sleep: (ms: number) => Promise<void>,
  maxMs = TILE_WAIT_MS,
  stepMs = TILE_POLL_MS,
): Promise<WebSettings> {
  let last: WebSettings = first;
  // (an older helper sends no tile_busy: nothing to wait for)
  for (let waited = 0; last.tile_busy === true && waited < maxMs; waited += stepMs) {
    await sleep(stepMs);
    try {
      last = await read();
    } catch {
      // keep the last answer; try again
    }
  }
  return last;
}

/** Turning LAN access on asks first (it opens the page to the network); turning it off just does it. */
export function lanToggleStep(currentlyOn: boolean): "confirm" | "apply" {
  return currentlyOn ? "apply" : "confirm";
}

/** A failed call as one short translated line; null for a 401 (the reload banner says it). */
export function settingsErrorText(err: unknown): string | null {
  if (err instanceof ApiError && err.status === 401) return null;
  if (isOffline(err)) return t("cw_home_err_offline");
  return t("cw_settings_err_save");
}

export interface SettingsViewProps {
  /** null until the first answer. */
  settings: WebSettings | null;
  loadFailed: boolean;
  /** "" = system default. */
  language: string;
  askLan: boolean;
  busy: boolean;
  error: string | null;
  onToggleLan: () => void;
  onConfirmLan: () => void;
  onDismissLan: () => void;
  onToggleTile: () => void;
  /** Choose the start page (#584): true opens on Cheats. Absent, or a helper without the setting: hidden. */
  onStartPage?: (cheats: boolean) => void;
  onLanguage: (code: string) => void;
  /** The Full screen switch: whether the page is full screen now, or null where the browser cannot
   *  or refused (the switch is hidden). */
  fullscreen?: boolean | null;
  onToggleFullscreen?: () => void;
  /** The page's theme and high-contrast switch (the Display section). */
  theme?: Theme;
  contrast?: boolean;
  onTheme?: (theme: Theme) => void;
  onToggleContrast?: () => void;
  /** The page's text size on this screen (#505). */
  textSize?: TextSize;
  onTextSize?: (size: TextSize) => void;
  /** The Controls screen and the first-run tips (App's overlays); each button shows only when given. */
  onOpenControls?: () => void;
  onShowTips?: () => void;
  /** Read /api/settings again after a failed load. */
  onRetry?: () => void;
  /** Turn off PS5Upload Minion: the confirm dialog is open (askOff), and its answers. */
  askOff?: boolean;
  onTurnOff?: () => void;
  onConfirmOff?: () => void;
  onDismissOff?: () => void;
  /** Minion was turned off from here: the page says so (and how to turn it back on) instead. */
  turnedOff?: boolean;
}

/** What the page shows once Minion was turned off from it: the server stops right after. */
export function TurnedOff() {
  return (
    <div className="cw-settings" data-setting="minion-off">
      <h1 className="cw-h1">{t("cw_minion_off_title")}</h1>
      <div className="cw-msg" role="status">
        {t("cw_settings_minion_off_confirm_body")}
      </div>
    </div>
  );
}

function Switch({
  action,
  label,
  on,
  disabled,
  onClick,
  note,
}: {
  action: string;
  label: string;
  on: boolean;
  disabled: boolean;
  onClick: () => void;
  note?: string;
}) {
  return (
    <div className="cw-setting">
      <button
        type="button"
        role="switch"
        aria-checked={on}
        disabled={disabled}
        className="cw-focusable cw-button"
        data-action={action}
        onClick={onClick}
      >
        <span>{label}</span>
        <span className={`cw-tag${on ? " is-on" : ""}`}>{t(on ? "cw_settings_on" : "cw_settings_off")}</span>
      </button>
      {note ? <p className="cw-dim cw-setting-note">{note}</p> : null}
    </div>
  );
}

/** A plain button row (Controls, Show tips again) with an optional note under it. */
function ActionRow({ setting, label, note, onClick }: { setting: string; label: string; note?: string; onClick: () => void }) {
  return (
    <div className="cw-setting" data-setting={setting}>
      <button type="button" className="cw-focusable cw-button" data-action={setting} onClick={onClick}>
        <span>{label}</span>
      </button>
      {note ? <p className="cw-dim cw-setting-note">{note}</p> : null}
    </div>
  );
}

/**
 * Settings without state: everything comes in through props (tests render it directly).
 * Order: Theme · Language · Full screen · Controls · Show tips again · Allow other devices ·
 * Show PS5Upload Minion on the home screen · Turn off PS5Upload Minion · About (each row carries
 * data-setting).
 */
export function SettingsView(p: SettingsViewProps) {
  const s = p.settings;
  if (p.turnedOff) return <TurnedOff />;
  return (
    <div className="cw-settings">
      <h1 className="cw-h1">{t("cw_nav_settings")}</h1>
      {p.error ? (
        <div className="cw-msg is-error" role="alert">
          {p.error}
        </div>
      ) : null}

      {p.theme ? (
        <section className="cw-home-section" data-setting="theme">
          <h2 className="cw-h2">{t("cw_settings_theme")}</h2>
          <div className="cw-quick" role="group" aria-label={t("cw_settings_theme")}>
            {THEMES.map((th) => (
              <button
                key={th}
                type="button"
                className="cw-focusable cw-button"
                data-theme-choice={th}
                aria-pressed={p.theme === th}
                onClick={() => p.onTheme?.(th)}
              >
                <span>{t(`cw_theme_${th}`)}</span>
              </button>
            ))}
          </div>
          {typeof p.contrast === "boolean" ? (
            <Switch
              action="contrast"
              label={t("cw_settings_contrast")}
              on={p.contrast}
              disabled={false}
              onClick={() => p.onToggleContrast?.()}
            />
          ) : null}
        </section>
      ) : null}

      {p.textSize ? (
        <section className="cw-home-section" data-setting="text-size">
          <h2 className="cw-h2">{t("cw_settings_text_size")}</h2>
          <div className="cw-quick" role="group" aria-label={t("cw_settings_text_size")}>
            {TEXT_SIZES.map((size) => (
              <button
                key={size}
                type="button"
                className="cw-focusable cw-button"
                data-text-size-choice={size}
                aria-pressed={p.textSize === size}
                onClick={() => p.onTextSize?.(size)}
              >
                <span>{`${size}%`}</span>
              </button>
            ))}
          </div>
          <p className="cw-dim cw-setting-note">{t("cw_settings_text_size_note")}</p>
        </section>
      ) : null}

      <section className="cw-home-section" data-setting="language">
        <h2 className="cw-h2">{t("cw_settings_language")}</h2>
        <div className="cw-quick" role="group" aria-label={t("cw_settings_language")}>
          {[""].concat(LANGUAGES).map((code) => (
            <button
              key={code || "system"}
              type="button"
              className="cw-focusable cw-button"
              data-lang={code || "system"}
              aria-pressed={p.language === code}
              onClick={() => p.onLanguage(code)}
            >
              <span>{code ? (LANGUAGE_NAMES[code] ?? code) : t("cw_settings_language_system")}</span>
            </button>
          ))}
        </div>
      </section>

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_settings_using")}</h2>
        {typeof p.fullscreen === "boolean" ? (
          <div data-setting="fullscreen">
            <Switch
              action="fullscreen"
              label={t("cw_settings_fullscreen")}
              on={p.fullscreen}
              disabled={false}
              onClick={() => p.onToggleFullscreen?.()}
              note={t("cw_settings_fullscreen_note")}
            />
          </div>
        ) : null}
        {p.onOpenControls ? (
          <ActionRow
            setting="controls"
            label={t("cw_settings_controls")}
            note={t("cw_settings_controls_note")}
            onClick={p.onOpenControls}
          />
        ) : null}
        {p.onShowTips ? <ActionRow setting="tips" label={t("cw_settings_tips")} onClick={p.onShowTips} /> : null}
      </section>

      {s && typeof s.start_cheats === "boolean" && p.onStartPage ? (
        <section className="cw-home-section" data-setting="start">
          <h2 className="cw-h2">{t("cw_settings_start")}</h2>
          <div className="cw-quick" role="group" aria-label={t("cw_settings_start")}>
            {[false, true].map((cheats) => (
              <button
                key={cheats ? "cheats" : "home"}
                type="button"
                className="cw-focusable cw-button"
                data-start-choice={cheats ? "cheats" : "home"}
                aria-pressed={s.start_cheats === cheats}
                onClick={() => p.onStartPage?.(cheats)}
              >
                <span>{t(cheats ? "cw_cheats_title" : "cw_nav_home")}</span>
              </button>
            ))}
          </div>
          <p className="cw-dim cw-setting-note">{t("cw_settings_start_note")}</p>
        </section>
      ) : null}

      <section className="cw-home-section">
        <h2 className="cw-h2">{t("cw_settings_network")}</h2>
        {s ? (
          <>
            <div data-setting="lan">
              <Switch
                action="lan"
                label={t("cw_settings_lan")}
                on={s.lan}
                disabled={p.busy}
                onClick={p.onToggleLan}
                note={t(s.lan ? "cw_settings_lan_off_note" : "cw_settings_lan_warning")}
              />
            </div>
            <div data-setting="tile">
              <Switch
                action="tile"
                label={t("cw_settings_tile")}
                on={s.tile}
                disabled={p.busy}
                onClick={p.onToggleTile}
                note={tileNote(s)}
              />
            </div>
            {canTurnOff(s) && p.onTurnOff ? (
              <ActionRow
                setting="minion-off"
                label={t("cw_settings_minion_off")}
                onClick={p.onTurnOff}
              />
            ) : null}
          </>
        ) : p.loadFailed ? (
          <div className="cw-msg is-error" role="alert">
            {t("cw_settings_err_load")}
            {p.onRetry ? (
              <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="retry" onClick={p.onRetry}>
                <span>{t("cw_files_retry")}</span>
              </button>
            ) : null}
          </div>
        ) : (
          <p className="cw-dim">{t("cw_home_loading")}</p>
        )}
      </section>

      <section className="cw-home-section" data-setting="about">
        <h2 className="cw-h2">{t("cw_settings_about")}</h2>
        <p className="cw-dim">
          {t("cw_settings_app_name")}
          {s ? ` · ${t("cw_settings_version", { version: s.version })}` : ""}
        </p>
      </section>

      {p.askLan ? (
        <ConfirmDialog
          title={t("cw_settings_lan_confirm_title")}
          body={t("cw_settings_lan_warning")}
          confirmLabel={t("cw_settings_lan_allow")}
          busy={p.busy}
          onConfirm={p.onConfirmLan}
          onCancel={p.onDismissLan}
        />
      ) : null}
      {p.askOff ? (
        <ConfirmDialog
          title={t("cw_settings_minion_off_confirm_title")}
          body={t("cw_settings_minion_off_confirm_body")}
          confirmLabel={t("cw_settings_minion_off_confirm")}
          danger
          busy={p.busy}
          onConfirm={() => p.onConfirmOff?.()}
          onCancel={() => p.onDismissOff?.()}
        />
      ) : null}
    </div>
  );
}

/** The Settings section: theme, language, full screen, controls and tips, network, About. */
export function Settings({ onOpenControls, onShowTips }: { onOpenControls?: () => void; onShowTips?: () => void } = {}) {
  const [settings, setSettings] = useState<WebSettings | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [askLan, setAskLan] = useState(false);
  const [askOff, setAskOff] = useState(false);
  const [turnedOff, setTurnedOff] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [language, setLanguageState] = useState(storedLanguage);
  const [fs, setFs] = useState(fullscreenState);
  // The switch shows the real state: entered, left (the browser's own way too) or refused.
  useEffect(() => watchFullscreen(setFs), []);
  const fullscreen = fs.supported ? fs.on : null;
  const [theme, setThemeState] = useState<Theme>(getTheme);
  const [contrast, setContrastState] = useState(getContrast);
  const [textSize, setTextSizeState] = useState<TextSize>(getTextSize);
  /** Bumped by Create (refresh) to read /api/settings again. */
  const [tick, setTick] = useState(0);

  useEffect(
    () =>
      onNavAction((a) => {
        if (a === "refresh") setTick((n) => n + 1);
        return false;
      }),
    [],
  );

  useEffect(() => {
    let live = true;
    setLoadFailed(false);
    call<WebSettings>("/api/settings").then(
      (r) => {
        if (live) setSettings(r);
      },
      () => {
        if (live) setLoadFailed(true);
      },
    );
    return () => {
      live = false;
    };
  }, [tick]);

  const live = useRef(true);
  useEffect(() => {
    // Set true here too: StrictMode runs the cleanup and then the effect again.
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  /** Turn off PS5Upload Minion: saved and answered before the helper stops this page. */
  const turnOff = useCallback(() => {
    setBusy(true);
    setError(null);
    call<WebSettings>("/api/settings/set", { enabled: false })
      .then(
        (s) => {
          if (!live.current) return;
          if (s.enabled === false) setTurnedOff(true);
          else setError(t("cw_settings_err_save"));
        },
        (e: unknown) => setError(settingsErrorText(e)),
      )
      .finally(() => {
        if (!live.current) return;
        setBusy(false);
        setAskOff(false);
      });
  }, []);

  const save = useCallback((change: Partial<Pick<WebSettings, "lan" | "tile" | "start_cheats">>) => {
    setBusy(true);
    setError(null);
    call<WebSettings>("/api/settings/set", change)
      .then(
        async (s) => {
          setSettings(s);
          if (change.lan !== undefined) forgetPhoneUrl(); // "Continue on your phone" asks again
          if (change.tile === undefined) return;
          // The tile is added or removed after the reply: wait for the real result (the switch stays
          // disabled meanwhile) and say so when it did not work.
          const done = await waitTileApplied(
            s,
            () => call<WebSettings>("/api/settings"),
            (ms) => new Promise((res) => setTimeout(res, ms)),
          );
          if (!live.current) return;
          setSettings(done);
          if (done.tile_failed) setError(t(done.tile ? "cw_settings_err_tile_add" : "cw_settings_err_tile_remove"));
        },
        (e: unknown) => setError(settingsErrorText(e)),
      )
      .finally(() => {
        if (!live.current) return;
        setBusy(false);
        setAskLan(false);
      });
  }, []);

  return (
    <SettingsView
      settings={settings}
      loadFailed={loadFailed}
      language={language}
      askLan={askLan}
      busy={busy}
      error={error}
      onToggleLan={() => {
        if (!settings || busy) return;
        if (lanToggleStep(settings.lan) === "confirm") setAskLan(true);
        else save({ lan: false });
      }}
      onConfirmLan={() => {
        if (!busy) save({ lan: true });
      }}
      onDismissLan={() => {
        if (!busy) setAskLan(false);
      }}
      onToggleTile={() => {
        if (settings && !busy) save({ tile: !settings.tile });
      }}
      onStartPage={(cheats) => {
        if (settings && !busy && settings.start_cheats !== cheats) save({ start_cheats: cheats });
      }}
      onLanguage={(code) => {
        chooseLanguage(code);
        setLanguageState(code);
      }}
      theme={theme}
      contrast={contrast}
      onTheme={(next) => {
        setTheme(next);
        setThemeState(next);
      }}
      onToggleContrast={() => {
        setContrast(!contrast);
        setContrastState(!contrast);
      }}
      textSize={textSize}
      onTextSize={(next) => {
        setTextSize(next);
        setTextSizeState(next);
      }}
      onOpenControls={onOpenControls}
      onShowTips={onShowTips}
      onRetry={() => setTick((n) => n + 1)}
      askOff={askOff}
      turnedOff={turnedOff}
      onTurnOff={() => {
        if (settings && !busy) setAskOff(true);
      }}
      onConfirmOff={() => {
        if (!busy) turnOff();
      }}
      onDismissOff={() => {
        if (!busy) setAskOff(false);
      }}
      fullscreen={fullscreen}
      onToggleFullscreen={() => {
        if (fullscreen !== null) setFullscreenPref(!fullscreen);
      }}
    />
  );
}
