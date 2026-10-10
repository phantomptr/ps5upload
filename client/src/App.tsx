import { readLastRoute } from "./lib/lastRoute";
import { Activity, Suspense, useEffect, useState, type ReactNode } from "react";
import {
  ConnectionScope,
  connectionSnapshotFor,
  useConnectionStore,
} from "./state/connection";
import { Navigate, Route, Routes, useLocation, type Location } from "react-router";

import { lazyWithReload } from "./lib/lazyWithReload";
import AppShell from "./layout/AppShell";
import { useRosterStore } from "./state/roster";
import { isTauriEnv } from "./lib/tauriEnv";

/**
 * Code-splitting strategy:
 *
 * Eagerly imported (always-on, small):
 *   - ChangelogScreen — landing route, must paint immediately
 *   - ConnectionScreen — first thing users see; adding suspense
 *     here would force a flash on app launch
 *   - SettingsScreen — small enough that lazy-loading isn't worth
 *     the suspense boundary
 *
 * Lazy-loaded via React.lazy (heavy or rarely-used):
 *   - everything else, especially Library (2.2k LOC), Upload, FileSystem
 *
 * Each lazy chunk is bundled as a separate JS file by Vite's default
 * rollup config — first navigation to e.g. /library downloads
 * library.chunk.js (~150 KB) instead of forcing every user to
 * download all 11 screens upfront.
 */
import ConnectionScreen from "./screens/Connection";
import ChangelogScreen from "./screens/Changelog";
import SettingsScreen from "./screens/Settings";
import HomeScreen from "./screens/Home";

const MoreScreen = lazyWithReload(() => import("./screens/More"));
const UploadScreen = lazyWithReload(() => import("./screens/Upload"));
const InstallPackageScreen = lazyWithReload(() => import("./screens/InstallPackage"));
const ConvertScreen = lazyWithReload(() => import("./screens/FpkgConvert"));
const GamesScreen = lazyWithReload(() => import("./screens/Games"));
const CollectionScreen = lazyWithReload(() => import("./screens/Collection"));
const SearchScreen = lazyWithReload(() => import("./screens/Search"));
const VolumesScreen = lazyWithReload(() => import("./screens/Volumes"));
const FileSystemScreen = lazyWithReload(() => import("./screens/FileSystem"));
const HardwareScreen = lazyWithReload(() => import("./screens/Hardware"));
const ProfileScreen = lazyWithReload(() => import("./screens/Profile"));
const BackupScreen = lazyWithReload(() => import("./screens/Backup"));
const LocalImageScreen = lazyWithReload(() => import("./screens/LocalImage"));
const HealthScreen = lazyWithReload(() => import("./screens/Health"));
const RemotePlayScreen = lazyWithReload(() => import("./screens/RemotePlay"));
const FanCurveScreen = lazyWithReload(() => import("./screens/FanCurve"));
const NotificationsScreen = lazyWithReload(() => import("./screens/Notifications"));
const CheatsScreen = lazyWithReload(() => import("./screens/Cheats"));
const GameActivityScreen = lazyWithReload(() => import("./screens/GameActivity"));
const GameHubScreen = lazyWithReload(() => import("./screens/GameHub"));
const FwSpoofScreen = lazyWithReload(() => import("./screens/FwSpoof"));
const ConnectionsScreen = lazyWithReload(() => import("./screens/Connections"));
const PayloadsScreen = lazyWithReload(() => import("./screens/Payloads"));
const FirstRunScreen = lazyWithReload(() => import("./screens/FirstRun"));
const SavesScreen = lazyWithReload(() => import("./screens/Saves"));
const ProcessesScreen = lazyWithReload(() => import("./screens/Processes"));
const CapturesScreen = lazyWithReload(() => import("./screens/Captures"));
const StatsScreen = lazyWithReload(() => import("./screens/Stats"));
const ShellScreen = lazyWithReload(() => import("./screens/Shell"));
const DiskUsageScreen = lazyWithReload(() => import("./screens/DiskUsage"));
const AboutScreen = lazyWithReload(() => import("./screens/About"));
const FAQScreen = lazyWithReload(() => import("./screens/FAQ"));
const LogsScreen = lazyWithReload(() => import("./screens/Logs"));
const ActivityScreen = lazyWithReload(() => import("./screens/Activity"));
const AuditLogScreen = lazyWithReload(() => import("./screens/AuditLog"));
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

/** /activity keeps working for old links (?console=… included) but lands on /tasks. */
function ActivityToTasks() {
  const { search, hash } = useLocation();
  return <Navigate to={{ pathname: "/tasks", search, hash }} replace />;
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
        <Route path="/whats-new" element={<ChangelogScreen />} />
        <Route path="/connection" element={<ConnectionScreen />} />
        {/* v5: mobile "everything else" hub. A real route (not a sheet)
             so the Android hardware back button and the backStack treat
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
          path="/upload"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <UploadScreen />
            </Suspense>
          }
        />
        <Route
          path="/install-package"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <InstallPackageScreen />
            </Suspense>
          }
        />
        <Route
          path="/convert"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ConvertScreen />
            </Suspense>
          }
        />
        <Route
          path="/games"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <GamesScreen />
            </Suspense>
          }
        />
        <Route
          path="/collection"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <CollectionScreen />
            </Suspense>
          }
        />
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
        <Route
          path="/search"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SearchScreen />
            </Suspense>
          }
        />
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
          path="/profile"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ProfileScreen />
            </Suspense>
          }
        />
        <Route
          path="/backup"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <BackupScreen />
            </Suspense>
          }
        />
        <Route
          path="/local-image"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <LocalImageScreen />
            </Suspense>
          }
        />
        <Route
          path="/health"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <HealthScreen />
            </Suspense>
          }
        />
        <Route
          path="/remote-play"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <RemotePlayScreen />
            </Suspense>
          }
        />
        <Route
          path="/fan-curve"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FanCurveScreen />
            </Suspense>
          }
        />
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
        <Route
          path="/game-activity"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <GameActivityScreen />
            </Suspense>
          }
        />
        <Route
          path="/fw-spoof"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FwSpoofScreen />
            </Suspense>
          }
        />
        {/* SMB Browser and FTP Server were replaced by Connections; old links land there. */}
        <Route path="/ftp-server" element={<Navigate to="/connections" replace />} />
        <Route
          path="/connections"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ConnectionsScreen />
            </Suspense>
          }
        />
        <Route path="/smb-browser" element={<Navigate to="/connections" replace />} />
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
            <NativeOnlyRoute>
              <Suspense fallback={<ScreenLoader />}>
                <PayloadsScreen />
              </Suspense>
            </NativeOnlyRoute>
          }
        />
        <Route path="/nanodns" element={<Navigate to="/payloads?tab=nanodns" replace />} />
        <Route path="/nano-dns" element={<Navigate to="/payloads?tab=nanodns" replace />} />
        <Route path="/shadowmount" element={<Navigate to="/payloads?tab=shadowmount" replace />} />
        {/* The wizard's whole point is step 2: download the payload ELFs to
            this machine and send them to the console over a raw socket.
            Neither is possible from a browser, and the /payloads entry it
            builds on is already hideInBrowser — so guard it the same way
            rather than stranding self-hosted users on a wizard that dies
            at step 2. */}
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
        {/* v5: /tasks is the canonical tasks/activity route. */}
        <Route
          path="/tasks"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityScreen />
            </Suspense>
          }
        />
        {/* Same screen; one address, so the sidebar's Tasks entry lights up for it. */}
        <Route path="/activity" element={<ActivityToTasks />} />
        <Route
          path="/stats"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <StatsScreen />
            </Suspense>
          }
        />
        {/* Legacy deep link / bookmark support for pre-2.12 installs.
            Kernel logs now live under the Logs tab ?tab=kernel.
            Keep the redirect indefinitely for any external bookmarks. */}
        <Route
          path="/kernel-log"
          element={<Navigate to="/logs?tab=kernel" replace />}
        />
        <Route
          path="/shell"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ShellScreen />
            </Suspense>
          }
        />
        <Route
          path="/disk-usage"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <DiskUsageScreen />
            </Suspense>
          }
        />
        {/* The old Dashboard was a smaller copy of Home. */}
        <Route path="/dashboard" element={<Navigate to="/home" replace />} />
        <Route
          path="/faq"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FAQScreen />
            </Suspense>
          }
        />
        <Route
          path="/logs"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <LogsScreen />
            </Suspense>
          }
        />
        <Route
          path="/audit-log"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <AuditLogScreen />
            </Suspense>
          }
        />
        <Route
          path="/bug-report"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <BugReportScreen />
            </Suspense>
          }
        />
        <Route path="/settings" element={<SettingsScreen />} />
        <Route
          path="/about"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <AboutScreen />
            </Suspense>
          }
        />
        <Route path="*" element={<Navigate to="/home" replace />} />
      </Route>
    </Routes>
  );
}
