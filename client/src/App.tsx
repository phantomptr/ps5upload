import { lazy, Suspense, type ReactNode } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
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

const UploadScreen = lazy(() => import("./screens/Upload"));
const InstallPackageScreen = lazy(() => import("./screens/InstallPackage"));
const LibraryScreen = lazy(() => import("./screens/Library"));
const InstalledAppsScreen = lazy(() => import("./screens/InstalledApps"));
const SearchScreen = lazy(() => import("./screens/Search"));
const VolumesScreen = lazy(() => import("./screens/Volumes"));
const FileSystemScreen = lazy(() => import("./screens/FileSystem"));
const HardwareScreen = lazy(() => import("./screens/Hardware"));
const ProfileScreen = lazy(() => import("./screens/Profile"));
const PayloadsScreen = lazy(() => import("./screens/Payloads"));
const NanoDnsScreen = lazy(() => import("./screens/NanoDns"));
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
  return <Navigate to={hasConsole ? "/whats-new" : "/connection"} replace />;
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
  return (
    <Routes>
      <Route element={<AppShell />}>
        {/* Landing: fresh installs go to Connection (see LandingRedirect);
         * returning users land on the changelog and route-restore takes
         * them back to their last screen. */}
        <Route index element={<LandingRedirect />} />
        <Route path="/whats-new" element={<ChangelogScreen />} />
        <Route path="/connection" element={<ConnectionScreen />} />
        <Route
          path="/upload"
          element={
            <NativeOnlyRoute>
              <Suspense fallback={<ScreenLoader />}>
                <UploadScreen />
              </Suspense>
            </NativeOnlyRoute>
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
          path="/library"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <LibraryScreen />
            </Suspense>
          }
        />
        <Route
          path="/installed"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <InstalledAppsScreen />
            </Suspense>
          }
        />
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
        <Route
          path="/file-system"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FileSystemScreen />
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
        <Route
          path="/nanodns"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <NanoDnsScreen />
            </Suspense>
          }
        />
        <Route
          path="/first-run"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <FirstRunScreen />
            </Suspense>
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
          path="/activity"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityScreen />
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
        <Route path="*" element={<Navigate to="/whats-new" replace />} />
      </Route>
    </Routes>
  );
}
