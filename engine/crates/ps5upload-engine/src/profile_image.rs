//! Avatar inputs shared by preview and apply.

pub(crate) fn read(input: &str) -> anyhow::Result<Vec<u8>> {
    // Desktop/Android pickers still pass an engine-readable path. Browser
    // pickers pass a data URL so a Docker engine never needs the user's path.
    if !input.starts_with("data:") {
        return std::fs::read(input).map_err(|e| anyhow::anyhow!("read image {input}: {e}"));
    }
    use base64::Engine as _;
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    anyhow::ensure!(
        input.len() <= MAX_BYTES.div_ceil(3) * 4 + 64,
        "Choose an image of 8 MiB or smaller"
    );
    let (header, data) = input
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("Invalid image data URL"))?;
    anyhow::ensure!(
        matches!(
            header,
            "data:image/png;base64"
                | "data:image/jpeg;base64"
                | "data:image/webp;base64"
                | "data:image/bmp;base64"
                | "data:image/gif;base64"
        ),
        "Unsupported image data URL"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| anyhow::anyhow!("Invalid base64 image data"))?;
    anyhow::ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_BYTES,
        "Choose an image of 8 MiB or smaller"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn browser_image_bytes_arrive_unchanged() {
        let bytes = b"browser-selected image bytes";
        let input = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        assert_eq!(read(&input).unwrap(), bytes);
    }

    #[test]
    fn browser_images_are_bounded_and_malformed_inputs_are_rejected() {
        for input in [
            "data:text/plain;base64,aGk=",
            "data:image/png;base64,???",
            "data:image/png,hello",
        ] {
            assert!(read(input).is_err());
        }
        let oversized = format!("data:image/png;base64,{}", "A".repeat(12 * 1024 * 1024));
        assert!(read(&oversized).is_err());
    }

    #[test]
    fn maximum_browser_image_fits_and_decoded_overflow_is_rejected() {
        let bytes = vec![0; 8 * 1024 * 1024];
        let input = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        assert_eq!(read(&input).unwrap(), bytes);
        // One extra byte can have the same encoded length as the exact limit.
        let oversized = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(vec![0; bytes.len() + 1])
        );
        assert!(read(&oversized).is_err());
    }

    #[tokio::test]
    async fn browser_preview_decodes_the_uploaded_image_and_rejects_bad_images() {
        use axum::response::IntoResponse;
        use image::ImageEncoder;
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        for mode in ["crop", "fit"] {
            let response =
                crate::profile_avatar_preview_handler(axum::Json(crate::ProfilePreviewReq {
                    image_path: format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(&png)
                    ),
                    mode: Some(mode.into()),
                }))
                .await
                .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let data = json["data_url"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/png;base64,")
                .unwrap();
            let preview = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            let img = image::load_from_memory(&preview).unwrap();
            assert_eq!((img.width(), img.height()), (440, 440));
        }
        let response =
            crate::profile_avatar_preview_handler(axum::Json(crate::ProfilePreviewReq {
                image_path: "data:image/png;base64,aGk=".into(),
                mode: None,
            }))
            .await
            .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn engine_file_paths_still_work() {
        let path = std::env::temp_dir().join(format!("avatar-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"native image").unwrap();
        let bytes = read(path.to_str().unwrap());
        std::fs::remove_file(path).unwrap();
        assert_eq!(bytes.unwrap(), b"native image");
    }
}
