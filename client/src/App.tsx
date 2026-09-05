import { lazy, Suspense, type ReactNode } from "react";
import { useConnectionStore } from "./state/connection";
import { Navigate, Route, Routes } from "react-router";
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

const MoreScreen = lazy(() => import("./screens/More"));
const UploadScreen = lazy(() => import("./screens/Upload"));
const InstallPackageScreen = lazy(() => import("./screens/InstallPackage"));
const GamesScreen = lazy(() => import("./screens/Games"));
const SearchScreen = lazy(() => import("./screens/Search"));
const VolumesScreen = lazy(() => import("./screens/Volumes"));
const FileSystemScreen = lazy(() => import("./screens/FileSystem"));
const HardwareScreen = lazy(() => import("./screens/Hardware"));
const ProfileScreen = lazy(() => import("./screens/Profile"));
const BackupScreen = lazy(() => import("./screens/Backup"));
const LocalImageScreen = lazy(() => import("./screens/LocalImage"));
const HealthScreen = lazy(() => import("./screens/Health"));
const RemotePlayScreen = lazy(() => import("./screens/RemotePlay"));
const FanCurveScreen = lazy(() => import("./screens/FanCurve"));
const NotificationsScreen = lazy(() => import("./screens/Notifications"));
const CheatsScreen = lazy(() => import("./screens/Cheats"));
const GameActivityScreen = lazy(() => import("./screens/GameActivity"));
const GameHubScreen = lazy(() => import("./screens/GameHub"));
const SdkChangerScreen = lazy(() => import("./screens/SdkChanger"));
const FakelibScreen = lazy(() => import("./screens/Fakelib"));
const FwSpoofScreen = lazy(() => import("./screens/FwSpoof"));
const FtpServerScreen = lazy(() => import("./screens/FtpServer"));
const SmbBrowserScreen = lazy(() => import("./screens/SmbBrowser"));
const PayloadsScreen = lazy(() => import("./screens/Payloads"));
const FirstRunScreen = lazy(() => import("./screens/FirstRun"));
const SavesScreen = lazy(() => import("./screens/Saves"));
const ProcessesScreen = lazy(() => import("./screens/Processes"));
const ScreenshotsScreen = lazy(() => import("./screens/Screenshots"));
const VideosScreen = lazy(() => import("./screens/Videos"));
const StatsScreen = lazy(() => import("./screens/Stats"));
const ShellScreen = lazy(() => import("./screens/Shell"));
const DiskUsageScreen = lazy(() => import("./screens/DiskUsage"));
const DashboardScreen = lazy(() => import("./screens/Dashboard"));
const AboutScreen = lazy(() => import("./screens/About"));
const FAQScreen = lazy(() => import("./screens/FAQ"));
const LogsScreen = lazy(() => import("./screens/Logs"));
const ActivityScreen = lazy(() => import("./screens/Activity"));
const AuditLogScreen = lazy(() => import("./screens/AuditLog"));
const BugReportScreen = lazy(() => import("./screens/BugReport"));

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
 * direction. Returning users keep the changelog landing, and AppShell's
 * route-restore then takes them to wherever they last worked (it
 * triggers on "/whats-new", not on "/connection", so this redirect
 * stays out of its way).
 */
function LandingRedirect() {
  const hasConsole = useRosterStore((s) => s.profiles.length > 0);
  return <Navigate to={hasConsole ? "/home" : "/connection"} replace />;
}

/** Guards a route whose screen has NO browser-functional path at all (see
 *  the matching `hideInBrowser` nav entry in Sidebar.tsx) — redirects a
 *  direct/typed navigation there in a browser session rather than rendering
 *  a screen with no working affordances. */
function NativeOnlyRoute({ children }: { children: ReactNode }) {
  if (!isTauriEnv()) return <Navigate to="/connection" replace />;
  return <>{children}</>;
}

export default function App() {
  // Screen state is per-console, and nothing from one console should
  // ever be shown against another.
  //
  // Individual screens guard their own in-flight calls with
  // useStaleHostGuard, but that only covers screens that remembered to
  // use it -- most do not, and every new screen starts out not using
  // it. Keying the whole route tree on the selected console closes the
  // class instead of patching it site by site: on a switch, React
  // unmounts every screen and remounts it fresh, so no cached list,
  // scan result, or error message can survive the change, and a reply
  // that arrives late lands on an unmounted component and is dropped.
  //
  // Transfers and queues are unaffected: they live in stores outside
  // the React tree, not in screen state.
  const host = useConnectionStore((s) => s.host);
  return (
    <Routes key={host || "no-console"}>
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
          path="/games"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <GamesScreen />
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
          path="/fakelib"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FakelibScreen />
            </Suspense>
          }
        />
        <Route
          path="/sdk-changer"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SdkChangerScreen />
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
        <Route
          path="/ftp-server"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FtpServerScreen />
            </Suspense>
          }
        />
        <Route
          path="/smb-browser"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SmbBrowserScreen />
            </Suspense>
          }
        />
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
          path="/screenshots"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ScreenshotsScreen />
            </Suspense>
          }
        />
        <Route
          path="/videos"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <VideosScreen />
            </Suspense>
          }
        />
        {/* v5: /tasks is the canonical tasks/activity route. */}
        <Route
          path="/tasks"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityScreen />
            </Suspense>
          }
        />
        <Route
          path="/activity"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityScreen />
            </Suspense>
          }
        />
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
        <Route
          path="/dashboard"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <DashboardScreen />
            </Suspense>
          }
        />
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
