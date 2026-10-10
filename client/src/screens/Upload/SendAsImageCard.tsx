import { useState } from "react";
import { useNavigate } from "react-router";
import { Layers } from "lucide-react";

import type { ImageFormat } from "../../api/fpkg";
import { Button, Checkbox, Select } from "../../components";
import { DEFAULT_IMAGE_SUBPATH } from "../../lib/imageUpload";
import { useConnectionStore } from "../../state/connection";
import { useFpkgConversion } from "../../state/fpkgConversion";
import { useTr } from "../../state/lang";

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

  const mine =
    pipeline.phase !== "idle" &&
    "source" in pipeline &&
    pipeline.source === sourcePath;
  const busyElsewhere = pipeline.phase === "running" && !mine;

  const start = () => {
    if (!host) return;
    const format: ImageFormat = choice === "exfat" ? "exfat" : "ffpkg";
    void buildImage(sourcePath, undefined, choice === "ffpfsc", format, {
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
          "upload_as_image_desc",
          undefined,
          "One image file that ShadowMount+ mounts, instead of thousands of files: far quicker to send, and compressed it takes less space on the PS5. The image is built on this computer first (it needs room here for it), then uploaded into the folder below; progress shows in Convert.",
        )}
      </p>
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
        <Button
          variant="primary"
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
                  "Building the image. It joins the Upload queue when it is ready.",
                )
              : pipeline.phase === "done"
                ? tr(
                    "upload_as_image_queued",
                    undefined,
                    "The image is built and in the Upload queue.",
                  )
                : pipeline.phase === "failed"
                  ? pipeline.message
                  : null}
          </span>
          <Button size="sm" onClick={() => navigate("/convert")}>
            {tr("upload_as_image_open_convert", undefined, "Open Convert")}
          </Button>
        </div>
      )}
    </section>
  );
}
