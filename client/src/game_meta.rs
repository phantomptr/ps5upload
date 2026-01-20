use crate::message::{CoverImage, GameMeta};
use image::GenericImageView;
use std::path::{Path, PathBuf};

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    let data = std::fs::read(path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn get_title_from_param(param: &serde_json::Value) -> Option<String> {
    if let Some(title) = param.get("titleName").and_then(|v| v.as_str()) {
        return Some(title.to_string());
    }
    let localized = param.get("localizedParameters")?;
    let mut region = localized
        .get("defaultLanguage")
        .and_then(|v| v.as_str())
        .unwrap_or("en-US")
        .trim();
    if region.is_empty() {
        region = "en-US";
    }
    let normalized = region.replace('_', "-");
    let fallback = localized
        .get("en-US")
        .and_then(|v| v.get("titleName"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    localized
        .get(&normalized)
        .and_then(|v| v.get("titleName"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or(fallback)
}

fn load_cover_image(path: &Path, max_dim: u32) -> Option<CoverImage> {
    let mut image = image::open(path).ok()?;
    let (w, h) = image.dimensions();
    let max_side = w.max(h);
    if max_side > max_dim {
        let scale = max_dim as f32 / max_side as f32;
        let nw = (w as f32 * scale).round().max(1.0) as u32;
        let nh = (h as f32 * scale).round().max(1.0) as u32;
        let resized =
            image::imageops::resize(&image, nw, nh, image::imageops::FilterType::Lanczos3);
        image = image::DynamicImage::ImageRgba8(resized);
    }
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(CoverImage {
        pixels: rgba.into_raw(),
        width: width as usize,
        height: height as usize,
    })
}

pub fn load_cover_image_from_bytes(bytes: &[u8], max_dim: u32) -> Option<CoverImage> {
    let mut image = image::load_from_memory(bytes).ok()?;
    let (w, h) = image.dimensions();
    let max_side = w.max(h);
    if max_side > max_dim {
        let scale = max_dim as f32 / max_side as f32;
        let nw = (w as f32 * scale).round().max(1.0) as u32;
        let nh = (h as f32 * scale).round().max(1.0) as u32;
        let resized =
            image::imageops::resize(&image, nw, nh, image::imageops::FilterType::Lanczos3);
        image = image::DynamicImage::ImageRgba8(resized);
    }
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(CoverImage {
        pixels: rgba.into_raw(),
        width: width as usize,
        height: height as usize,
    })
}

fn is_rar_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("rar"))
        .unwrap_or(false)
}

fn find_param_path(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let sce = path.join("sce_sys").join("param.json");
        if sce.exists() {
            return Some(sce);
        }
        let direct = path.join("param.json");
        if direct.exists() {
            return Some(direct);
        }
        return None;
    }

    let parent = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy().to_string();
    if !stem.is_empty() {
        let candidate = parent.join(&stem);
        let sce = candidate.join("sce_sys").join("param.json");
        if sce.exists() {
            return Some(sce);
        }
        let direct = candidate.join("param.json");
        if direct.exists() {
            return Some(direct);
        }
    }
    None
}

pub fn load_game_meta_from_rar(path: &Path) -> Option<(GameMeta, Option<CoverImage>)> {
    let (param, cover) = crate::unrar::probe_rar_local(path)?;
    let param = param?;
    let meta = parse_game_meta_from_param_bytes(&param)?;
    let cover = cover
        .as_deref()
        .and_then(|bytes| load_cover_image_from_bytes(bytes, 160));
    Some((meta, cover))
}

pub fn parse_game_meta_from_param_bytes(param_bytes: &[u8]) -> Option<GameMeta> {
    let param: serde_json::Value = serde_json::from_slice(param_bytes).ok()?;
    let title = get_title_from_param(&param).unwrap_or_else(|| "Unknown".to_string());
    let title_id = param
        .get("titleId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content_id = param
        .get("contentId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = param
        .get("contentVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(GameMeta {
        title,
        title_id,
        content_id,
        version,
    })
}

pub fn load_game_meta_for_path(path: &str) -> (Option<GameMeta>, Option<CoverImage>) {
    if path.trim().is_empty() {
        return (None, None);
    }
    let path = Path::new(path);
    if path.is_file() && is_rar_path(path) {
        if let Some((meta, cover)) = load_game_meta_from_rar(path) {
            return (Some(meta), cover);
        }
    }
    let param_path = match find_param_path(path) {
        Some(p) => p,
        None => return (None, None),
    };
    let param = match read_json_file(&param_path) {
        Some(v) => v,
        None => return (None, None),
    };
    let title = get_title_from_param(&param).unwrap_or_else(|| "Unknown".to_string());
    let title_id = param
        .get("titleId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content_id = param
        .get("contentId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = param
        .get("contentVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let meta = GameMeta {
        title,
        title_id,
        content_id,
        version,
    };

    let mut cover = None;
    let candidates = [
        "icon0.png",
        "icon0.jpg",
        "icon0.jpeg",
        "icon.png",
        "cover.png",
        "cover.jpg",
        "tile0.png",
    ];
    let sce_sys_dir = if param_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("sce_sys")
    {
        param_path.parent().map(|p| p.to_path_buf())
    } else {
        param_path.parent().map(|p| p.join("sce_sys"))
    };
    if let Some(sce_sys) = sce_sys_dir {
        for name in candidates.iter() {
            let c = sce_sys.join(name);
            if c.exists() {
                cover = load_cover_image(&c, 160);
                if cover.is_some() {
                    break;
                }
            }
        }
    }
    if cover.is_none() {
        let game_root = if param_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("sce_sys")
        {
            param_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
        } else {
            param_path.parent().map(|p| p.to_path_buf())
        };
        if let Some(root) = game_root {
            for name in candidates.iter() {
                let c = root.join(name);
                if c.exists() {
                    cover = load_cover_image(&c, 160);
                    if cover.is_some() {
                        break;
                    }
                }
            }
        }
    }

    (Some(meta), cover)
}
