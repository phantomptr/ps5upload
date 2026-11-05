// Where a queued game's image goes. One answer for the row that shows it and the runner that uses it.

import { destinationVolumeFor, useUploadStore } from "../state/upload";
import { DEFAULT_IMAGE_SUBPATH } from "./imageUpload";

export interface ImageDest {
  volume: string | null;
  subpath: string;
}

/** A game's own destination, or for one with none, what is set for its console now. */
export function imageDestFor(own: ImageDest | null | undefined, host: string): ImageDest {
  return (
    own ?? {
      volume: destinationVolumeFor(host),
      subpath: useUploadStore.getState().destinationSubpath || DEFAULT_IMAGE_SUBPATH,
    }
  );
}
