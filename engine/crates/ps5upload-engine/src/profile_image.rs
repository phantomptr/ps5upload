//! Avatar inputs shared by preview and apply.

pub(crate) fn read(input: &str) -> anyhow::Result<Vec<u8>> {
    Ok(std::fs::read(input)?)
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
    fn engine_file_paths_still_work() {
        let path = std::env::temp_dir().join(format!("avatar-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"native image").unwrap();
        let bytes = read(path.to_str().unwrap());
        std::fs::remove_file(path).unwrap();
        assert_eq!(bytes.unwrap(), b"native image");
    }
}
