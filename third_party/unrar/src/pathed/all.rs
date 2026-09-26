use std::path::{Path, PathBuf};
use widestring::{WideCString, WideCStr};

pub(crate) type RarString = WideCString;
pub(crate) type RarStr = WideCStr;

pub(crate) fn construct(path: &Path) -> RarString {
    WideCString::from_os_str(path).expect("Unexpected nul in path")
}

pub(crate) fn process_file(
    handle: *const unrar_sys::Handle,
    operation: i32,
    dest_path: Option<&RarStr>,
    dest_name: Option<&RarStr>,
) -> i32 {
    unsafe {
        unrar_sys::RARProcessFileW(
            handle,
            operation,
            dest_path.map(|path| path.as_ptr().cast()).unwrap_or(std::ptr::null()),
            dest_name.map(|file| file.as_ptr().cast()).unwrap_or(std::ptr::null()),
        )
    }
}

pub(crate) fn preprocess_extract(
    base: Option<&Path>,
    _filename: &PathBuf,
) -> (Option<RarString>, Option<RarString>) {
    (base.map(construct), None)
}
