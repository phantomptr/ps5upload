use std::path::PathBuf;

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let logo_path = manifest_dir.join("..").join("logo.png");
    if !logo_path.exists() {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon_path = out_dir.join("ps5upload.ico");

    if let Err(err) = build_ico(&logo_path, &icon_path) {
        eprintln!("Failed to build Windows icon: {}", err);
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon(icon_path.to_string_lossy().as_ref());
    let _ = res.compile();
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
