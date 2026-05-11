import { lazy, Suspense } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import AppShell from "./layout/AppShell";

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
const SearchScreen = lazy(() => import("./screens/Search"));
const VolumesScreen = lazy(() => import("./screens/Volumes"));
const FileSystemScreen = lazy(() => import("./screens/FileSystem"));
const HardwareScreen = lazy(() => import("./screens/Hardware"));
const SendPayloadScreen = lazy(() => import("./screens/SendPayload"));
const PayloadsScreen = lazy(() => import("./screens/Payloads"));
const FirstRunScreen = lazy(() => import("./screens/FirstRun"));
const SavesScreen = lazy(() => import("./screens/Saves"));
const ScreenshotsScreen = lazy(() => import("./screens/Screenshots"));
const StatsScreen = lazy(() => import("./screens/Stats"));
const KernelLogScreen = lazy(() => import("./screens/KernelLog"));
const ShellScreen = lazy(() => import("./screens/Shell"));
const DiskUsageScreen = lazy(() => import("./screens/DiskUsage"));
const DashboardScreen = lazy(() => import("./screens/Dashboard"));
const AboutScreen = lazy(() => import("./screens/About"));
const FAQScreen = lazy(() => import("./screens/FAQ"));
const LogsScreen = lazy(() => import("./screens/Logs"));
const ActivityScreen = lazy(() => import("./screens/Activity"));

/**
 * Suspense fallback. Deliberately minimal — a spinner would
 * compete with the screen content that's about to render. The empty
 * div maintains layout space without flashing visual noise; chunks
 * load in <200ms on a typical LAN-attached install.
 */
function ScreenLoader() {
  return <div className="flex h-full items-center justify-center" />;
}

export default function App() {
  return (
    <Routes>
      <Route element={<AppShell />}>
        {/* Landing page: Changelog. New users see "what is this app"
         * framed as a fresh-changes list, and returning users catch
         * up on updates before diving back into work. */}
        <Route index element={<Navigate to="/whats-new" replace />} />
        <Route path="/whats-new" element={<ChangelogScreen />} />
        <Route path="/connection" element={<ConnectionScreen />} />
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
          path="/library"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <LibraryScreen />
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
          path="/send-payload"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <SendPayloadScreen />
            </Suspense>
          }
        />
        <Route
          path="/payloads"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <PayloadsScreen />
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
          path="/screenshots"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ScreenshotsScreen />
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
        <Route
          path="/kernel-log"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <KernelLogScreen />
            </Suspense>
          }
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
          path="/activity"
          element={
            <Suspense fallback={<ScreenLoader />}>
              <ActivityScreen />
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
