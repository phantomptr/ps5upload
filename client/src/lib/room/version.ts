// The app's version for a room knock and the "Works with my version" filter: only the `version`
// field of package.json goes into a bundle (a named JSON import), not the rest of the file.
import { version } from "../../../package.json";

export const APP_VERSION: string = version;
