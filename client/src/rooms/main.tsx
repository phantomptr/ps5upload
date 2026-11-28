// The website's Rooms page (https://ps5upload.phantomptr.com/rooms/, built by vite.rooms.config.ts
// into site/rooms/index.html): the public Play Rooms from the hub's cached list, with the same
// sorts and filters as the apps. Joining hands over to /join/ with the room in the fragment: the
// same join flow as an invite (name → password if any → waiting → playing). Nothing here is
// served by us besides the static file; the hub answers this origin's CORS.
import "../lib/polyfills";
import { createRoot } from "react-dom/client";
import "../index.css";
import { RoomsPage } from "./RoomsPage";
import { installArrowNav } from "../lib/tvNav";

installArrowNav();
createRoot(document.getElementById("root")!).render(<RoomsPage />);
