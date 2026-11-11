import { useState } from "react";
import { useNavigate } from "react-router";
import { Layers } from "lucide-react";

import type { ImageFormat } from "../../api/fpkg";
import { Button, Checkbox, Select } from "../../components";
import { DEFAULT_IMAGE_SUBPATH, imageDrive } from "../../lib/imageUpload";
import { pickPath } from "../../lib/pickPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useConnectionStore } from "../../state/connection";
import { useConvertPrefs } from "../../state/convertPrefs";
import { pickLocalPath } from "../../state/localPicker";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";
import { imageFolderPath, useConsoleVolumes } from "../FpkgConvert/ImageDestination";

type Choice = "ffpfsc" | "ffpkg" | "exfat";

/** A game folder sent as one image instead of thousands of files: Convert builds it on this
 *  computer, then it joins the Upload queue by itself. */
export function SendAsImageCard({
  sourcePath,
  destinationVolume,
  destinationSubpath,
}: {
  sourcePath: string;
  destinationVolume: string | null;
  destinationSubpath: string;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((c) =>
    c.payloadStatus === "up" ? (c.host?.trim() ?? "") : "",
  );
  const pipeline = useFpkgConversion((s) => s.pipeline);
  const buildImage = useFpkgConversion((s) => s.buildImage);
  const [choice, setChoice] = useState<Choice>("ffpfsc");
  const [deleteAfter, setDeleteAfter] = useState(true);
  const [started, setStarted] = useState(false);
  // The image is built in Workshop's Output folder (the same setting, changed here or there):
  // before, it always went to the engine's default, ~/Downloads/fpkgs.
  const outputDir = useConvertPrefs((s) => s.outputDir);
  const setOutputDir = useConvertPrefs((s) => s.setOutputDir);
  const changeOutput = async () => {
    try {
      const picked = !isTauriEnv()
        ? await pickLocalPath({
            mode: "folder",
            title: tr("fpkg.pickOutputHost", undefined, "Choose an output folder on the engine host"),
          })
        : await pickPath({ mode: "folder", title: tr("fpkg.pickOutput", undefined, "Choose an output folder") });
      if (picked) setOutputDir(picked);
    } catch {
      // Closed, or no picker here: the folder stays as it was.
    }
  };

  const mine =
    pipeline.phase !== "idle" &&
    "source" in pipeline &&
    pipeline.source === sourcePath;
  const busyElsewhere = pipeline.phase === "running" && !mine;

  // With no drive picked, an image goes to this console's drive for packages (#673).
  const volumes = useConsoleVolumes(host);
  const goesTo = host ? `${imageFolderPath(imageDrive(host, destinationVolume, volumes).volume, destinationSubpath)}/…` : null;

  const start = () => {
    if (!host) return;
    const format: ImageFormat = choice === "exfat" ? "exfat" : "ffpkg";
    void buildImage(sourcePath, outputDir.trim() || undefined, choice === "ffpfsc", format, {
      host,
      volume: destinationVolume,
      subpath: destinationSubpath || DEFAULT_IMAGE_SUBPATH,
      deleteAfter,
    });
    setStarted(true);
  };

  return (
    <section className="rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] mb-4 p-5">
      <div className="flex items-center gap-2 font-medium">
        <Layers size={16} />
        {tr("upload_as_image_title", undefined, "Or send it as one game image")}
      </div>
      <p className="mt-1 text-xs text-[var(--color-muted)]">
        {tr(
          "upload_as_image_desc_v2",
          undefined,
          "One image file that ShadowMount+ mounts, instead of thousands of files: far quicker to send, and compressed it takes less space on the PS5. The image is built on this computer first (it needs room here for it), then uploaded into the folder below; progress shows in Workshop.",
        )}
      </p>
      {goesTo && (
        <p className="mt-1 break-all text-xs text-[var(--color-muted)]" data-testid="send-as-image-path">
          {tr("image_dest_goes_to", { path: goesTo }, `Goes to ${goesTo}`)}
        </p>
      )}
      <div className="mt-3 flex flex-wrap items-end gap-3">
        <Select
          block={false}
          label={tr("upload_as_image_format", undefined, "Image")}
          value={choice}
          onChange={(e) => setChoice(e.target.value as Choice)}
          options={[
            {
              value: "ffpfsc",
              label: tr(
                "upload_as_image_ffpfsc",
                undefined,
                "Compressed (.ffpfsc), smallest",
              ),
            },
            {
              value: "ffpkg",
              label: tr(
                "upload_as_image_ffpkg",
                undefined,
                "UFS2 (.ffpkg), what ShadowMount+ recommends",
              ),
            },
            {
              value: "exfat",
              label: tr("upload_as_image_exfat", undefined, "exFAT image (.exfat)"),
            },
          ]}
        />
        {/* The alternative to the Upload button below, so not filled: one primary per screen. */}
        <Button
          variant="secondary"
          disabled={
            !host ||
            busyElsewhere ||
            (started && mine && pipeline.phase === "running")
          }
          onClick={start}
        >
          {tr(
            "upload_as_image_go",
            undefined,
            "Build the image, then upload it",
          )}
        </Button>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-[var(--color-muted)]">
        <span>{tr("fpkg.output", undefined, "Output folder")}:</span>
        <span className="min-w-0 basis-[12rem] break-all font-mono" data-testid="upload-image-output">
          {outputDir.trim() || tr("fpkg.outputHint", undefined, "Default: ~/Downloads/fpkgs")}
        </span>
        <Button variant="ghost" size="sm" onClick={() => void changeOutput()}>
          {tr("fpkg.change", undefined, "Change")}
        </Button>
      </div>
      <Checkbox
        className="mt-3"
        checked={deleteAfter}
        onChange={setDeleteAfter}
        label={tr(
          "upload_as_image_delete",
          undefined,
          "Delete the image from this computer once it is on the PS5",
        )}
      />
      {!host && (
        <p className="mt-2 text-xs text-[var(--color-muted)]">
          {tr(
            "fpkg.uploadNeedsPs5",
            undefined,
            "Connect to a PS5 to upload it.",
          )}
        </p>
      )}
      {busyElsewhere && (
        <p className="mt-2 text-xs text-[var(--color-warn)]">
          {tr(
            "upload_as_image_busy",
            undefined,
            "Convert is building another game. Try again when it is done.",
          )}
        </p>
      )}
      {started && mine && (
        <div className="mt-3 flex flex-wrap items-center gap-2 text-sm">
          <span
            className={
              pipeline.phase === "failed"
                ? "text-[var(--color-bad)]"
                : "text-[var(--color-good)]"
            }
          >
            {pipeline.phase === "running"
              ? tr(
                  "upload_as_image_building",
                  undefined,
                  "Building the image. It joins the queue when it is ready.",
                )
              : pipeline.phase === "done"
                ? tr(
                    "upload_as_image_queued",
                    undefined,
                    "The image is built and in the queue.",
                  )
                : pipeline.phase === "failed"
                  ? pipeline.message
                  : null}
          </span>
          <Button size="sm" onClick={() => navigate("/workshop")}>
            {tr("open_workshop", undefined, "Open Workshop")}
          </Button>
        </div>
      )}
    </section>
  );
}
