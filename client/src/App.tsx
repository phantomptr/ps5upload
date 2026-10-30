import { readLastRoute } from "./lib/lastRoute";
import { Activity, Suspense, useEffect, useRef, useState, type ReactNode } from "react";
import {
  ConnectionScope,
  connectionSnapshotFor,
  useConnectionStore,
} from "./state/connection";
import { Navigate, Route, Routes, useLocation, useNavigate, type Location } from "react-router";
import { recallRoute, rememberRoute } from "./lib/consoleRoutes";
import { payloadCardLink, RUNNING_LINK } from "./screens/Payloads/payloadTabs";

import { lazyWithReload } from "./lib/lazyWithReload";
import AppShell from "./layout/AppShell";
import { useRosterStore } from "./state/roster";
import { isTauriEnv } from "./lib/tauriEnv";

/**
 * Code-splitting strategy:
 *
 * Eagerly imported (always-on, small):
 *   - HomeScreen — where returning users land
 *
 * Lazy-loaded via React.lazy (heavy or rarely-used):
 *   - everything else, especially Library (2.2k LOC), Upload, FileSystem
 *
 * Each lazy chunk is bundled as a separate JS file by Vite's default
 * rollup config — first navigation to e.g. /library downloads
 * library.chunk.js (~150 KB) instead of forcing every user to
 * download all 11 screens upfront.
 */
import HomeScreen from "./screens/Home";
import { ConnectHub } from "./screens/Connection/ConnectHub";

// Settings (2k lines) and Connection pull in a lot; nobody needs them
// before the first paint of Home.
const ConnectionScreen = lazyWithReload(() => import("./screens/Connection"));
const DevicesScreen = lazyWithReload(() => import("./screens/Devices"));
const SharedFoldersScreen = lazyWithReload(() => import("./screens/SharedFolders"));
const SettingsScreen = lazyWithReload(() => import("./screens/Settings"));

const MoreScreen = lazyWithReload(() => import("./screens/More"));
const SendScreen = lazyWithReload(() => import("./screens/Send"));
const WorkshopScreen = lazyWithReload(() => import("./screens/FpkgConvert"));
const GamesScreen = lazyWithReload(() => import("./screens/Games"));
const VolumesScreen = lazyWithReload(() => import("./screens/Volumes"));
const FileSystemScreen = lazyWithReload(() => import("./screens/FileSystem"));
const HardwareScreen = lazyWithReload(() => import("./screens/Hardware"));
const AccountsScreen = lazyWithReload(() => import("./screens/Accounts"));
const PlayScreen = lazyWithReload(() => import("./screens/Play"));
const PlaySetupScreen = lazyWithReload(() => import("./screens/Play/setup"));
const PadRemoteScreen = lazyWithReload(() => import("./screens/Play/PadRemote"));
const HealthScreen = lazyWithReload(() => import("./screens/Health"));
const NotificationsScreen = lazyWithReload(() => import("./screens/Notifications"));
const CheatsScreen = lazyWithReload(() => import("./screens/Cheats"));
const GameHubScreen = lazyWithReload(() => import("./screens/GameHub"));
const PayloadsScreen = lazyWithReload(() => import("./screens/Payloads"));
const FirstRunScreen = lazyWithReload(() => import("./screens/FirstRun"));
const SavesScreen = lazyWithReload(() => import("./screens/Saves"));
const ProcessesScreen = lazyWithReload(() => import("./screens/Processes"));
const CapturesScreen = lazyWithReload(() => import("./screens/Captures"));
const ShellScreen = lazyWithReload(() => import("./screens/Shell"));
const CleanupScreen = lazyWithReload(() => import("./screens/Cleanup"));
const HelpScreen = lazyWithReload(() => import("./screens/Help"));
const ActivityLogsScreen = lazyWithReload(() => import("./screens/ActivityLogs"));
const BugReportScreen = lazyWithReload(() => import("./screens/BugReport"));

/**
 * Suspense fallback. Deliberately minimal — a spinner would
 * compete with the screen content that's about to render. The empty
 * div maintains layout space without flashing visual noise; chunks
 * load in <200ms on a typical LAN-attached install.
 */
function ScreenLoader() {
  return <div className="flex h-full items-center justify-center" />;
}

/**
 * Landing logic (v3): a fresh install — no console in the roster yet —
 * goes straight to Connection, because nothing in the app works before
 * a console is set up and "What's new" gave first-time users zero
 * direction. Returning users go back to the screen they were last on
 * (saved by AppShell, see lib/lastRoute), or Home.
 */
function LandingRedirect() {
  const hasConsole = useRosterStore((s) => s.profiles.length > 0);
  // A returning user reopens the screen they were last on.
  const to = hasConsole ? (readLastRoute() ?? "/home") : "/connection";
  return <Navigate to={to} replace />;
}

/** A screen folded into another: an old link (its query kept, so `?add=1` still opens the
 *  form) lands on the screen that holds it now, on its section when `hash` names one. */
function MovedTo({ to, hash }: { to: string; hash?: string }) {
  const location = useLocation();
  return (
    <Navigate to={{ pathname: to, search: location.search, hash: hash ? `#${hash}` : location.hash }} replace />
  );
}

/** An old /devices or /health link: Connection's tab, with the rest of its query kept. */
function ToConnectionTab({ tab }: { tab: "devices" | "health" }) {
  const { search, hash } = useLocation();
  const params = new URLSearchParams(search);
  params.set("tab", tab);
  return <Navigate to={{ pathname: "/connection", search: `?${params}`, hash }} replace />;
}

/** /activity keeps working for old links (?console=… included) but lands on /tasks. */
function ActivityToTasks() {
  const { search, hash } = useLocation();
  return <Navigate to={{ pathname: "/tasks", search, hash }} replace />;
}

/** Logs is a tab of Activity & logs; its old `?tab=kernel` is that tab's `log=kernel`. */
function LogsToTasks() {
  const { search } = useLocation();
  const kernel = new URLSearchParams(search).get("tab") === "kernel";
  return <Navigate to={kernel ? "/tasks?tab=logs&log=kernel" : "/tasks?tab=logs"} replace />;
}

/** Guards a route whose screen has NO browser-functional path at all (see
 *  the matching `hideInBrowser` nav entry in Sidebar.tsx) — redirects a
 *  direct/typed navigation there in a browser session rather than rendering
 *  a screen with no working affordances. */
function NativeOnlyRoute({ children }: { children: ReactNode }) {
  if (!isTauriEnv()) return <Navigate to="/connection" replace />;
  return <>{children}</>;
}

/** How many consoles keep their screens alive at once (the selected one included). */
const KEPT_CONSOLES = 3;

const NO_CONSOLE = "no-console";

/** Each console keeps its own screen (#518): the route is remembered for the console it was
 *  used with, and switching to a console goes back to where it was left (across restarts, for
 *  any number of consoles). Nothing moves the first time a console is used here, or when its
 *  screen is the one already shown. Here, above the per-console trees: inside one, the console
 *  never changes. */
function useConsoleRouteMemory() {
  const location = useLocation();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const lastHost = useRef(host);
  const route = location.pathname + location.search;
  useEffect(() => {
    const before = lastHost.current;
    if (host !== before) {
      lastHost.current = host;
      // A switch from one console to another (not the first console set at start).
      if (before && host) {
        const back = recallRoute(host);
        if (back && back !== route) {
          navigate(back);
          return; // the new route is remembered when it lands
        }
      }
    }
    if (host) rememberRoute(host, route);
  }, [host, route, navigate]);
}

export default function App() {
  // Screen state is per-console, and nothing from one console should
  // ever be shown against another.
  //
  // Each console the user selects gets its own tree of screens. Only the
  // selected console's tree is on show; the others stay mounted but
  // hidden, so switching console tabs and back loses nothing that was
  // typed, opened or loaded. A hidden tree runs no effects (no timers,
  // no requests), reads the connection state as it was when its console
  // was last selected (ConnectionScope), and keeps the address it was
  // last on. So it cannot turn into the selected console, and a reply
  // that arrives late lands on the tree of the console that asked.
  //
  // Transfers and queues are unaffected: they live in stores outside
  // the React tree, not in screen state.
  const host = useConnectionStore((s) => s.host);
  useConsoleRouteMemory();
  const here = host || NO_CONSOLE;
  const location = useLocation();
  const [kept, setKept] = useState<Array<{ key: string; location: Location }>>([]);
  useEffect(() => {
    // Most recent last; the console selected longest ago is let go beyond the limit.
    setKept((prev) =>
      [...prev.filter((k) => k.key !== here), { key: here, location }].slice(-KEPT_CONSOLES),
    );
  }, [here, location]);
  // The console being selected is not in `kept` until the effect above has run.
  const trees = kept.some((k) => k.key === here) ? kept : [...kept, { key: here, location }];
  return (
    <>
      {[...trees]
        .sort((a, b) => a.key.localeCompare(b.key))
        .map(({ key, location: last }) => {
          const active = key === here;
          const treeHost = key === NO_CONSOLE ? "" : key;
          // A hidden tree with nothing remembered for its console would read the selected
          // console's state: better gone than wrong.
          if (!active && !connectionSnapshotFor(treeHost)) return null;
          return (
            <Activity key={key} mode={active ? "visible" : "hidden"}>
              <ConnectionScope host={treeHost} frozen={!active}>
                {/* Always given a location, so the tree keeps one shape whether shown or hidden. */}
                <AppRoutes location={active ? location : last} />
              </ConnectionScope>
            </Activity>
          );
        })}
    </>
  );
}

function AppRoutes({ location }: { location: Location }) {
  return (
    <Routes location={location}>
      <Route element={<AppShell />}>
        {/* Landing: fresh installs go to Connection (see LandingRedirect);
         * returning users land on the changelog and route-restore takes
         * them back to their last screen. */}
        <Route index element={<LandingRedirect />} />
        <Route path="/home" element={<HomeScreen />} />
        <Route path="/whats-new" element={<Navigate to="/faq?tab=whats-new" replace />} />
        <Route
          path="/connection"
          element={
            <ConnectHub
              console={
                <Suspense fallback={<ScreenLoader />}>
                  <ConnectionScreen />
                </Suspense>
              }
              devices={
                <Suspense fallback={<ScreenLoader />}>
                  <DevicesScreen />
                </Suspense>
              }
              health={
                <Suspense fallback={<ScreenLoader />}>
                  <HealthScreen />
                </Suspense>
              }
            />
          }
        />
        {/* Devices and Health Check are tabs of Connection now. */}
        <Route path="/devices" element={<ToConnectionTab tab="devices" />} />
        <Route path="/health" element={<ToConnectionTab tab="health" />} />
        <Route
          path="/shared-folders"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SharedFoldersScreen />
            </Suspense>
          }
        />
        {/* Mesh Drive and Network shares are one Shared Folders screen now. */}
        <Route path="/mesh-drive" element={<MovedTo to="/shared-folders" hash="mesh-drive" />} />
        <Route path="/connections" element={<MovedTo to="/shared-folders" />} />
        {/* v5: mobile "everything else" hub. A real route (not a sheet)
             so the Android hardware back button and router history treat
             it like any other screen. */}
        <Route
          path="/more"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <MoreScreen />
            </Suspense>
          }
        />
        <Route
          path="/send"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SendScreen />
            </Suspense>
          }
        />
        {/* Upload and Install Package are one Send to PS5 screen now. */}
        <Route path="/upload" element={<MovedTo to="/send" />} />
        <Route path="/install-package" element={<MovedTo to="/send" />} />
        <Route
          path="/workshop"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <WorkshopScreen />
            </Suspense>
          }
        />
        {/* Convert Games and Game images are one Workshop screen now; Game images is its
            Look Inside view. */}
        <Route path="/convert" element={<MovedTo to="/workshop" />} />
        <Route path="/local-image" element={<Navigate to="/workshop?view=inside" replace />} />
        <Route
          path="/games"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <GamesScreen />
            </Suspense>
          }
        />
        {/* The whole library is the Games screen's Everything view now. */}
        <Route path="/collection" element={<Navigate to="/games?tab=all" replace />} />
        {/* v5 Game Hub: everything about one game behind one URL. */}
        <Route
          path="/games/:title_id"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <GameHubScreen />
            </Suspense>
          }
        />
        {/* v5: /games is now the canonical games grid. /library redirects
             for backward compatibility with deep links and bookmarks. */}
        <Route path="/library" element={<Navigate to="/games?tab=files" replace />} />
        <Route path="/installed" element={<Navigate to="/games?tab=ready" replace />} />
        {/* Search, What takes space? and Snapshots are views of Files now. */}
        <Route path="/search" element={<Navigate to="/files?view=search" replace />} />
        <Route
          path="/volumes"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <VolumesScreen />
            </Suspense>
          }
        />
        {/* v5: /files is the canonical file browser route. /file-system
             redirects for backward compatibility. */}
        <Route
          path="/files"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FileSystemScreen />
            </Suspense>
          }
        />
        <Route path="/file-system" element={<Navigate to="/files" replace />} />
        {/* v5: /console is the canonical console-management route. */}
        <Route
          path="/console"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <HardwareScreen />
            </Suspense>
          }
        />
        <Route
          path="/hardware"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <HardwareScreen />
            </Suspense>
          }
        />
        <Route
          path="/accounts"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <AccountsScreen />
            </Suspense>
          }
        />
        {/* Profile and Remote Play are one Accounts screen now. */}
        <Route path="/profile" element={<Navigate to="/accounts" replace />} />
        <Route path="/remote-play" element={<Navigate to="/play/setup" replace />} />
        <Route
          path="/play"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <PlayScreen />
            </Suspense>
          }
        />
        {/* Setting up Play, step by step (10.0.0). */}
        <Route
          path="/play/setup"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <PlaySetupScreen />
            </Suspense>
          }
        />
        {/* This phone as the controller of another device's Remote Play session. */}
        <Route
          path="/play/pad"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <PadRemoteScreen />
            </Suspense>
          }
        />
        <Route path="/backup" element={<Navigate to="/files?view=snapshots" replace />} />
        {/* The console takes one fan target temperature, set on the Console
            screen's fan card; a multi-point curve had nothing to drive. */}
        <Route path="/fan-curve" element={<Navigate to="/console" replace />} />
        <Route
          path="/notifications"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <NotificationsScreen />
            </Suspense>
          }
        />
        <Route
          path="/cheats"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <CheatsScreen />
            </Suspense>
          }
        />
        {/* Play time is on Games (Recently played) and each game's page now. */}
        <Route path="/game-activity" element={<Navigate to="/games" replace />} />
        {/* The firmware spoof check is a card on Console now. */}
        <Route path="/fw-spoof" element={<Navigate to="/console#fw-spoof" replace />} />
        {/* SMB Browser and FTP Server were replaced by Connections; old links land there. */}
        <Route path="/ftp-server" element={<Navigate to="/shared-folders" replace />} />
        <Route path="/smb-browser" element={<Navigate to="/shared-folders" replace />} />
        {/* Legacy deep link / bookmark support for pre-2.12 installs.
            The Payloads tab now owns send functionality under ?tab=send.
            Keep the redirect indefinitely for any external bookmarks. */}
        <Route
          path="/send-payload"
          element={<Navigate to="/payloads?tab=send" replace />}
        />
        <Route
          path="/payloads"
          element={
            // The browser build gets the "On your PS5" tab only
            // (screens/Payloads/payloadTabs.ts).
            <Suspense fallback={<ScreenLoader />}>
              <PayloadsScreen />
            </Suspense>
          }
        />
        {/* Integrations were tabs (and before that screens); they are cards on "On your
            PS5" now, and Processes and Ports are one Running tab of Payloads. Old links open the card
            or the tab. */}
        <Route path="/nanodns" element={<Navigate to={payloadCardLink("nanodns")} replace />} />
        <Route path="/nano-dns" element={<Navigate to={payloadCardLink("nanodns")} replace />} />
        <Route path="/shadowmount" element={<Navigate to={payloadCardLink("shadowmountplus")} replace />} />
        <Route path="/hearbridge" element={<Navigate to={payloadCardLink("hearbridge")} replace />} />
        <Route path="/payloads/manager" element={<Navigate to={payloadCardLink("payload-manager")} replace />} />
        <Route path="/payloads/shadowmount" element={<Navigate to={payloadCardLink("shadowmountplus")} replace />} />
        <Route path="/payloads/nanodns" element={<Navigate to={payloadCardLink("nanodns")} replace />} />
        <Route path="/payloads/hearbridge" element={<Navigate to={payloadCardLink("hearbridge")} replace />} />
        <Route path="/payloads/ports" element={<Navigate to={RUNNING_LINK} replace />} />
        <Route path="/ports" element={<Navigate to={RUNNING_LINK} replace />} />
        {/* The wizard's whole point is step 2: download the payload ELFs to
            this machine and send them to the console over a raw socket.
            Neither is possible from a browser (the Payloads screen there has
            no Catalog or Send tab), so guard it rather than stranding
            self-hosted users on a wizard that dies at step 2. */}
        <Route
          path="/first-run"
          element={
            <NativeOnlyRoute>
              <Suspense fallback={<ScreenLoader />}>
                <FirstRunScreen />
              </Suspense>
            </NativeOnlyRoute>
          }
        />
        <Route
          path="/saves"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SavesScreen />
            </Suspense>
          }
        />
        <Route
          path="/processes"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ProcessesScreen />
            </Suspense>
          }
        />
        <Route
          path="/captures"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <CapturesScreen />
            </Suspense>
          }
        />
        {/* Screenshots and Video clips were two screens; old links and bookmarks still land. */}
        <Route path="/screenshots" element={<Navigate to="/captures" replace />} />
        <Route path="/videos" element={<Navigate to="/captures?tab=videos" replace />} />
        {/* Activity & logs: Tasks, Logs and the Audit log as tabs. */}
        <Route
          path="/tasks"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityLogsScreen />
            </Suspense>
          }
        />
        {/* Same screen; one address, so the sidebar's Tasks entry lights up for it. */}
        <Route path="/activity" element={<ActivityToTasks />} />
        {/* Stats is a tab of Tasks now. */}
        <Route path="/stats" element={<Navigate to="/tasks?tab=stats" replace />} />
        {/* Pre-2.12 bookmarks: kernel logs are Activity & logs › Logs › Kernel. */}
        <Route path="/kernel-log" element={<Navigate to="/tasks?tab=logs&log=kernel" replace />} />
        <Route
          path="/shell"
          element={
            <NativeOnlyRoute>
              <Suspense fallback={<ScreenLoader />}>
                <ShellScreen />
              </Suspense>
            </NativeOnlyRoute>
          }
        />
        <Route path="/disk-usage" element={<Navigate to="/files?view=space&path=/user" replace />} />
        <Route
          path="/cleanup"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <CleanupScreen />
            </Suspense>
          }
        />
        {/* The old Dashboard was a smaller copy of Home. */}
        <Route path="/dashboard" element={<Navigate to="/home" replace />} />
        <Route
          path="/faq"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <HelpScreen />
            </Suspense>
          }
        />
        {/* Logs and the Audit log are tabs of Activity & logs; FAQ, About and What's new of Help. */}
        <Route path="/logs" element={<LogsToTasks />} />
        <Route path="/audit-log" element={<Navigate to="/tasks?tab=audit" replace />} />
        <Route path="/about" element={<Navigate to="/faq?tab=about" replace />} />
        <Route
          path="/bug-report"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <BugReportScreen />
            </Suspense>
          }
        />
        <Route
          path="/settings"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SettingsScreen />
            </Suspense>
          }
        />
        <Route path="*" element={<Navigate to="/home" replace />} />
      </Route>
    </Routes>
  );
}
