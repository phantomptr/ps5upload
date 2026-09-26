use std::ffi::{CString, CStr};
use std::path::{Path, PathBuf};

pub(crate) type RarString = CString;
pub(crate) type RarStr = CStr;

pub(crate) fn construct<P: AsRef<std::path::Path>>(path: P) -> RarString {
    CString::new(path.as_ref().as_os_str().as_encoded_bytes()).unwrap()
}

pub(crate) fn process_file(
    handle: *const unrar_sys::Handle,
    operation: i32,
    dest_path: Option<&RarStr>,
    dest_name: Option<&RarStr>,
) -> i32 {
    unsafe {
        unrar_sys::RARProcessFile(
            handle,
            operation,
            dest_path.map(|path| path.as_ptr().cast()).unwrap_or(std::ptr::null()),
            dest_name.map(|file| file.as_ptr().cast()).unwrap_or(std::ptr::null()),
        )
    }
}

pub(crate) fn preprocess_extract(
    base: Option<&Path>,
    filename: &PathBuf,
) -> (Option<RarString>, Option<RarString>) {
    (None, Some(construct(base.unwrap_or(".".as_ref()).join(filename))))
}
