import type { LinkInstallMode } from "../state/linkInstallPrefs";

/** The three ways to install from a link, the default first. */
export const LINK_MODES: readonly LinkInstallMode[] = [
  "stream",
  "direct",
  "download",
];

/** What a link mode asks of the user, as facts the screen words and the confirm repeats.
 *  One place, so the mode picker, the certificate option and the confirm cannot disagree
 *  (they did: every mode's confirm said "this computer downloads the package"). */
export interface LinkModeFacts {
  /** Who fetches the link. */
  downloader: "ps5" | "computer";
  computerMustStayAwake: boolean;
  /** Room for the whole package is needed on this computer. */
  needsDiskHere: boolean;
  /** This app can show the transfer's progress (otherwise only the PS5 does). */
  progressHere: boolean;
  /** "Skip the certificate check" has an effect: only where this computer makes the
   *  connection. The PS5 does its own TLS handshake in direct mode. */
  certificateCheckApplies: boolean;
}

export function linkModeFacts(mode: LinkInstallMode): LinkModeFacts {
  if (mode === "direct") {
    return {
      downloader: "ps5",
      computerMustStayAwake: false,
      needsDiskHere: false,
      progressHere: false,
      certificateCheckApplies: false,
    };
  }
  return {
    downloader: "computer",
    computerMustStayAwake: true,
    needsDiskHere: mode === "download",
    progressHere: true,
    certificateCheckApplies: true,
  };
}

/** The way a link is really installed: a server that sends the file only whole (no byte
 *  ranges) can be neither streamed to the PS5 nor fetched by it, so this computer saves the
 *  package first (#872). `info` null: the link could not be checked, the choice stands. */
export function linkModeFor(
  chosen: LinkInstallMode,
  info: { kind: string; ranges: boolean } | null,
): LinkInstallMode {
  return info?.kind === "pkg" && info.ranges === false ? "download" : chosen;
}
