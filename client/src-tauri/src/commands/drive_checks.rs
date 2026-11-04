//! Whether a mounted volume is one the USB autoloader wizard may offer. Its list used to differ
//! per OS: Windows hid USB SSDs and hard drives (they report as fixed disks), macOS and Linux
//! offered network shares and read-only volumes, which then failed with "Read-only file system".
//! When a check cannot be made the volume stays on the list: writing to it then says what is wrong.

/// A mount a stick's files can go on: on this computer and writable, by `statfs` flags.
#[cfg(any(target_os = "macos", test))]
fn mac_flags_usable(flags: u32, local: u32, read_only: u32) -> bool {
    flags & local != 0 && flags & read_only == 0
}

/// A local, writable mount: not an SMB share, not a mounted disk image.
#[cfg(target_os = "macos")]
pub(super) fn usable_mount(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `statfs` fills the zeroed struct on success, and only then is it read.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut info) } != 0 {
        return true;
    }
    mac_flags_usable(
        info.f_flags,
        libc::MNT_LOCAL as u32,
        libc::MNT_RDONLY as u32,
    )
}

/// A writable mount. (Which of them are network shares Linux does not say so simply, and the
/// folders scanned are the ones removable media are mounted under.)
#[cfg(target_os = "linux")]
pub(super) fn usable_mount(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `statvfs` fills the zeroed struct on success, and only then is it read.
    let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut info) } != 0 {
        return true;
    }
    info.f_flag & libc::ST_RDONLY == 0
}

/// `STORAGE_BUS_TYPE::BusTypeUsb`.
#[cfg(any(target_os = "windows", test))]
const BUS_TYPE_USB: u32 = 7;

/// The bus in a `STORAGE_DEVICE_DESCRIPTOR`: a u32 after `Version`, `Size`, four one-byte fields
/// and four offsets. None when the reply is too short to hold it.
#[cfg(any(target_os = "windows", test))]
fn descriptor_bus_type(descriptor: &[u8]) -> Option<u32> {
    let bytes = descriptor.get(28..32)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// Is the drive at `root` ("E:\") the system's own? It may be on USB too (Windows To Go).
#[cfg(any(target_os = "windows", test))]
fn is_system_root(root: &str, system_drive: Option<&str>) -> bool {
    system_drive.is_some_and(|sys| {
        root.trim_end_matches('\\')
            .eq_ignore_ascii_case(sys.trim_end_matches('\\'))
    })
}

/// A fixed disk connected over USB. Windows reports USB SSDs and hard drives as fixed, and only
/// sticks as removable, but they are just what the wizard is for.
#[cfg(target_os = "windows")]
pub(super) fn fixed_disk_on_usb(root: &str) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    extern "system" {
        fn DeviceIoControl(
            device: *mut std::ffi::c_void,
            code: u32,
            input: *const std::ffi::c_void,
            input_len: u32,
            output: *mut std::ffi::c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }

    if is_system_root(root, std::env::var("SystemDrive").ok().as_deref()) {
        return false;
    }
    // `\\.\E:`: the volume itself. Opened with no access to its data, which needs no rights.
    let volume = format!(r"\\.\{}", root.trim_end_matches('\\'));
    let Ok(device) = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&volume)
    else {
        return false;
    };
    // STORAGE_PROPERTY_QUERY: StorageDeviceProperty, PropertyStandardQuery, one unused byte.
    let query: [u32; 3] = [0, 0, 0];
    let mut reply = [0u8; 256];
    let mut returned = 0u32;
    // SAFETY: both buffers outlive the call and their lengths are the ones passed; no overlapped
    // I/O is asked for, so nothing writes to them afterwards.
    let ok = unsafe {
        DeviceIoControl(
            device.as_raw_handle().cast(),
            IOCTL_STORAGE_QUERY_PROPERTY,
            query.as_ptr().cast(),
            std::mem::size_of_val(&query) as u32,
            reply.as_mut_ptr().cast(),
            reply.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    ok != 0 && descriptor_bus_type(&reply[..returned as usize]) == Some(BUS_TYPE_USB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_volume_must_be_local_and_writable() {
        const LOCAL: u32 = 0x1000;
        const READ_ONLY: u32 = 0x1;
        assert!(mac_flags_usable(LOCAL, LOCAL, READ_ONLY), "a USB drive");
        assert!(!mac_flags_usable(0, LOCAL, READ_ONLY), "an SMB share");
        assert!(
            !mac_flags_usable(LOCAL | READ_ONLY, LOCAL, READ_ONLY),
            "a mounted disk image"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_writable_folder_counts_and_a_missing_one_stays_on_the_list() {
        assert!(usable_mount(&std::env::temp_dir()));
        assert!(usable_mount(std::path::Path::new("/no/such/volume")));
    }

    #[test]
    fn the_bus_is_read_from_its_place_in_the_descriptor() {
        let mut descriptor = [0u8; 40];
        descriptor[28] = BUS_TYPE_USB as u8;
        assert_eq!(descriptor_bus_type(&descriptor), Some(BUS_TYPE_USB));
        descriptor[28] = 11; // SATA
        assert_ne!(descriptor_bus_type(&descriptor), Some(BUS_TYPE_USB));
        assert_eq!(descriptor_bus_type(&descriptor[..30]), None, "cut short");
    }

    #[test]
    fn the_system_drive_is_never_offered_even_on_usb() {
        assert!(is_system_root(r"C:\", Some("C:")));
        assert!(is_system_root(r"c:\", Some("C:")));
        assert!(!is_system_root(r"E:\", Some("C:")));
        assert!(!is_system_root(r"E:\", None));
    }
}
