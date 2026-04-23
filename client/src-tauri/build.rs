// Tauri build-script hook. Invokes the Tauri codegen so the renderer's
// `invoke()` calls get their command schemas, and embeds the window-icon
// + bundle metadata. Must stay synchronous / std-only.
fn main() {
    tauri_build::build();
}
