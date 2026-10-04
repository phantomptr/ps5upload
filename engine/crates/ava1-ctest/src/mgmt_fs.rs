//! P3 Task 4: the native filesystem runners (`payload/src/mgmt_fs.c`) and the log/net/node
//! runners over stub handlers, installed behind a temp-directory path policy (csrc/test_shim_fs.c).
#![cfg(unix)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;

extern "C" {
    fn ava1_test_mgmtfs_install(root: *const c_char) -> c_int;
    fn ava1_test_mgmtfs_uninstall();
    fn ava1_test_mgmtfs_allow_dev(on: c_int);
    fn ava1_test_path_tree_op_refused(p: *const c_char) -> c_int;
    fn ava1_test_path_in_protected(p: *const c_char) -> c_int;
    fn ava1_test_path_contains_protected(p: *const c_char) -> c_int;
    fn ava1_test_mgmtfs_set(fake_dev: c_int, klog_avail: u32, syslog_len: u32);
    fn ava1_test_mgmtfs_stats(counted: *mut u32, shutdowns: *mut u32, unsafe_seen: *mut u32);
}

/// Installs the table; every fs method acts only under `root` (the temp dir of the test).
pub fn install(root: &Path) -> i32 {
    let c = CString::new(root.to_str().unwrap()).unwrap();
    unsafe { ava1_test_mgmtfs_install(c.as_ptr()) }
}

pub fn uninstall() {
    unsafe { ava1_test_mgmtfs_uninstall() }
}

/// `fake_dev`: a path containing `/mnt2` is on another device (the rename guard's input).
/// `klog_avail` / `syslog_len`: how much text the stub log handlers hold (`u32::MAX` = fail).
pub fn set(fake_dev: bool, klog_avail: u32, syslog_len: u32) {
    unsafe { ava1_test_mgmtfs_set(fake_dev as c_int, klog_avail, syslog_len) }
}

/// Fake devices where a link named `lnk` points into the other device: `stat` (following it) says
/// device 2, `lstat` (the link itself) says device 1; `/mnt2` is device 2.
pub fn set_link_devices() {
    unsafe { ava1_test_mgmtfs_set(2, 0, 0) }
}

/// Every device lookup fails (the rename guard cannot tell): the move must be refused.
pub fn set_unreadable_devices() {
    unsafe { ava1_test_mgmtfs_set(3, 0, 0) }
}

/// (commands counted by fs methods, node.shutdown handler calls, reads that asked for FSR_UNSAFE).
pub fn stats() -> (u32, u32, u32) {
    let (mut a, mut b, mut c) = (0, 0, 0);
    unsafe { ava1_test_mgmtfs_stats(&mut a, &mut b, &mut c) };
    (a, b, c)
}

/// Lets the path policy accept `/dev/...` too (the symlink-source rename test).
pub fn allow_dev(on: bool) {
    unsafe { ava1_test_mgmtfs_allow_dev(on as c_int) }
}

/// `path_in_protected` (the trust-store check every policy runs): the path is the protected
/// directory (`<root>/d/ava`) or below it.
pub fn in_protected(p: &str) -> bool {
    let c = CString::new(p).unwrap();
    unsafe { ava1_test_path_in_protected(c.as_ptr()) != 0 }
}

/// `path_contains_protected`: the path is the protected directory or one of its ancestors.
pub fn contains_protected(p: &str) -> bool {
    let c = CString::new(p).unwrap();
    unsafe { ava1_test_path_contains_protected(c.as_ptr()) != 0 }
}

/// `path_tree_op_refused`: the shared refusal for a path that a recursive/moving operation would reach the
/// trust store through (the store, below it, or an ancestor).
pub fn tree_op_refused(p: &str) -> bool {
    let c = CString::new(p).unwrap();
    unsafe { ava1_test_path_tree_op_refused(c.as_ptr()) != 0 }
}
