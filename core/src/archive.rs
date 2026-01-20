use std::path::Path;
use walkdir::WalkDir;

pub fn get_size(path: &str) -> u64 {
    let path = Path::new(path);
    if path.is_file() {
        if let Ok(meta) = path.metadata() {
            return meta.len();
        }
    } else {
        return WalkDir::new(path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
    }
    0
}
