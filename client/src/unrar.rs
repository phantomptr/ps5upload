use std::path::Path;

#[cfg(feature = "client-unrar")]
mod native {
    use super::Path;
    use std::ffi::CString;

    use libc::{c_char, c_int, size_t};

    extern "C" {
        fn unrar_probe_archive(
            rar_path: *const c_char,
            param_buf: *mut *mut c_char,
            param_size: *mut size_t,
            cover_buf: *mut *mut c_char,
            cover_size: *mut size_t,
        ) -> c_int;
    }

    const UNRAR_OK: c_int = 0;

    pub fn probe_rar_local(path: &Path) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        if !path.is_file() {
            return None;
        }
        let path_str = path.to_string_lossy();
        let c_path = CString::new(path_str.as_bytes()).ok()?;

        let mut param_ptr: *mut c_char = std::ptr::null_mut();
        let mut cover_ptr: *mut c_char = std::ptr::null_mut();
        let mut param_size: size_t = 0;
        let mut cover_size: size_t = 0;

        let result = unsafe {
            unrar_probe_archive(
                c_path.as_ptr(),
                &mut param_ptr,
                &mut param_size,
                &mut cover_ptr,
                &mut cover_size,
            )
        };
        if result != UNRAR_OK {
            unsafe {
                if !param_ptr.is_null() {
                    libc::free(param_ptr as *mut libc::c_void);
                }
                if !cover_ptr.is_null() {
                    libc::free(cover_ptr as *mut libc::c_void);
                }
            }
            return None;
        }

        let param = unsafe {
            if !param_ptr.is_null() && param_size > 0 {
                let slice =
                    std::slice::from_raw_parts(param_ptr as *const u8, param_size as usize);
                let vec = slice.to_vec();
                libc::free(param_ptr as *mut libc::c_void);
                Some(vec)
            } else {
                if !param_ptr.is_null() {
                    libc::free(param_ptr as *mut libc::c_void);
                }
                None
            }
        };

        let cover = unsafe {
            if !cover_ptr.is_null() && cover_size > 0 {
                let slice =
                    std::slice::from_raw_parts(cover_ptr as *const u8, cover_size as usize);
                let vec = slice.to_vec();
                libc::free(cover_ptr as *mut libc::c_void);
                Some(vec)
            } else {
                if !cover_ptr.is_null() {
                    libc::free(cover_ptr as *mut libc::c_void);
                }
                None
            }
        };

        Some((param, cover))
    }
}

#[cfg(not(feature = "client-unrar"))]
pub fn probe_rar_local(_path: &Path) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    None
}

#[cfg(feature = "client-unrar")]
pub use native::probe_rar_local;
