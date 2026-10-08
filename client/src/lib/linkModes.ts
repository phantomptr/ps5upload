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
