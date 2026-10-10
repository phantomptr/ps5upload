import { Activity } from "react";
import { useSearchParams } from "react-router";
import { Image as ImageIcon, Video as VideoIcon } from "lucide-react";

import { PageHeader } from "../../components";
import { Tabs } from "../../components/Tabs";
import { useTr } from "../../state/lang";
import ScreenshotsScreen from "../Screenshots";
import VideosScreen from "../Videos";

export type CapturesTab = "screenshots" | "videos";

/** Which tab an address asks for: `?tab=videos`, anything else is screenshots. */
export function capturesTabOf(params: URLSearchParams): CapturesTab {
  return params.get("tab") === "videos" ? "videos" : "screenshots";
}

/**
 * The PS5's Capture Gallery in one place: screenshots and video clips, a tab each.
 *
 * Both lists stay mounted (the hidden one is paused, not thrown away), so a selection, a
 * scroll position or a download in one survives a look at the other.
 */
export default function CapturesScreen() {
  const tr = useTr();
  const [params, setParams] = useSearchParams();
  const tab = capturesTabOf(params);
  const tabs = (
    <>
      <PageHeader
        icon={ImageIcon}
        title={tr("captures", undefined, "Screenshots & clips")}
      />
      <Tabs
        className="mb-4"
        variant="segmented"
        ariaLabel={tr("captures_title", undefined, "Captures")}
        value={tab}
        onChange={(id) =>
          setParams(id === "videos" ? { tab: "videos" } : {}, { replace: true })
        }
        tabs={[
          {
            id: "screenshots",
            label: tr("screenshots_title", undefined, "Screenshots"),
            icon: ImageIcon,
          },
          {
            id: "videos",
            label: tr("videos_title", undefined, "Video clips"),
            icon: VideoIcon,
          },
        ]}
      />
    </>
  );
  return (
    <>
      <Activity mode={tab === "screenshots" ? "visible" : "hidden"}>
        <ScreenshotsScreen tabs={tabs} />
      </Activity>
      <Activity mode={tab === "videos" ? "visible" : "hidden"}>
        <VideosScreen tabs={tabs} />
      </Activity>
    </>
  );
}
