use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    write_chat_key(&manifest_dir, &out_dir);

    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let logo_path = manifest_dir.join("..").join("logo.png");
    if !logo_path.exists() {
        return;
    }

    let icon_path = out_dir.join("ps5upload.ico");

    if let Err(err) = build_ico(&logo_path, &icon_path) {
        eprintln!("Failed to build Windows icon: {}", err);
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon(icon_path.to_string_lossy().as_ref());
    res.set_manifest(r#"
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
        <requestedPrivileges>
            <requestedExecutionLevel level="asInvoker" uiAccess="false" />
        </requestedPrivileges>
    </security>
</trustInfo>
</assembly>
"#);
    let _ = res.compile();
}

fn write_chat_key(manifest_dir: &PathBuf, out_dir: &PathBuf) {
    let key_path = Some(manifest_dir.join("ps5upload_chat.key"));
    let key = key_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let key = key.trim();
    let key_literal = key.replace('\\', "\\\\").replace('"', "\\\"");
    let content = format!("pub const CHAT_SHARED_KEY_HEX: &str = \"{}\";\n", key_literal);
    let out_file = out_dir.join("chat_key.rs");
    let _ = std::fs::write(out_file, content);
    if let Some(path) = key_path {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn build_ico(src: &PathBuf, dst: &PathBuf) -> Result<(), String> {
    let image = image::open(src).map_err(|e| e.to_string())?;
    let sizes = [16u32, 24, 32, 48, 64, 128, 256];
    let mut frames = Vec::new();
    for size in sizes {
        let resized = image::imageops::resize(&image, size, size, image::imageops::FilterType::Lanczos3);
        let (w, h) = resized.dimensions();
        let buf = resized.into_raw();
        let frame = image::codecs::ico::IcoFrame::as_png(&buf, w, h, image::ExtendedColorType::Rgba8)
            .map_err(|e| e.to_string())?;
        frames.push(frame);
    }
    image::codecs::ico::IcoEncoder::new(std::fs::File::create(dst).map_err(|e| e.to_string())?)
        .encode_images(&frames)
        .map_err(|e| e.to_string())
}
