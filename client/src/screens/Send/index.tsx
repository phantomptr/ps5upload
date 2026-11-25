import { Send as SendIcon } from "lucide-react";

import { ConsoleChip, PageHeader } from "../../components";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import InstallPackageScreen from "../InstallPackage";
import { FpkgNote } from "../InstallPackage/FpkgNote";
import UploadScreen from "../Upload";

/**
 * Upload & Install (route /send): every way something gets onto the console, on one page, top
 * to bottom.
 *
 * 0. The fake-package (FPKG) note, first: PS5 fake games play up to 13.60, and above 11.60 only
 *    with a kstuff build that supports fake packages there, which matters before anything is sent.
 * 1. From this computer or a saved server: the drop zone is the one way in. Pick or drop
 *    anything (a package, a game folder or dump, an image, an archive, any file); the app works
 *    out what it is and offers what fits it, a package's Stream & install included. It also
 *    opens the link and on-PS5 installs below.
 * 2. From a link, or an archive of packages.
 * 3. The one queue, for this console.
 * 4. Already on the PS5: packages on a USB drive and package copies put there before.
 *
 * Upload and Install Package were two screens that both installed packages (both offered
 * Stream & install and Upload & install), so people had to know which one their file
 * belonged to. Old /upload and /install-package links land here.
 */
export default function SendScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  return (
    <div className="app-page">
      <PageHeader
        icon={SendIcon}
        title={tr("send_title_v2", undefined, "Upload & Install")}
        description={tr(
          "send_description2",
          undefined,
          "Install packages on the PS5, and copy games and files to it.",
        )}
        right={<ConsoleChip addr={host} />}
      />
      <FpkgNote />
      <UploadScreen embedded />
      <InstallPackageScreen embedded />
    </div>
  );
}
