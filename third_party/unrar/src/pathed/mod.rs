#[cfg_attr(any(target_os = "linux", target_os = "netbsd"), path = "linux.rs")]
#[cfg_attr(not(any(target_os = "linux", target_os = "netbsd")), path = "all.rs")]
mod os;

pub(crate) use os::*;
