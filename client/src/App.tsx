import { Navigate, Route, Routes } from "react-router-dom";
import AppShell from "./layout/AppShell";
import ConnectionScreen from "./screens/Connection";
import UploadScreen from "./screens/Upload";
import LibraryScreen from "./screens/Library";
import SearchScreen from "./screens/Search";
import VolumesScreen from "./screens/Volumes";
import FileSystemScreen from "./screens/FileSystem";
import HardwareScreen from "./screens/Hardware";
import SettingsScreen from "./screens/Settings";
import SendPayloadScreen from "./screens/SendPayload";
import AboutScreen from "./screens/About";
import ChangelogScreen from "./screens/Changelog";
import FAQScreen from "./screens/FAQ";
import LogsScreen from "./screens/Logs";
import ActivityScreen from "./screens/Activity";

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
        <Route path="/upload" element={<UploadScreen />} />
        <Route path="/library" element={<LibraryScreen />} />
        <Route path="/search" element={<SearchScreen />} />
        <Route path="/volumes" element={<VolumesScreen />} />
        <Route path="/file-system" element={<FileSystemScreen />} />
        <Route path="/hardware" element={<HardwareScreen />} />
        <Route path="/send-payload" element={<SendPayloadScreen />} />
        <Route path="/faq" element={<FAQScreen />} />
        <Route path="/logs" element={<LogsScreen />} />
        <Route path="/activity" element={<ActivityScreen />} />
        <Route path="/settings" element={<SettingsScreen />} />
        <Route path="/about" element={<AboutScreen />} />
        <Route path="*" element={<Navigate to="/whats-new" replace />} />
      </Route>
    </Routes>
  );
}
