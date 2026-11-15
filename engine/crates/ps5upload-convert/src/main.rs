//! `ps5upload-convert`: see the library's docs.
//!
//!   ps5upload-convert inspect <path>   the inspection as one line of JSON on stdout
//!   ps5upload-convert job <folder>     run the request in <folder> (how the helper uses it)
//!
//! On the PS5, `payload/convert/entry.c` runs first and, for a start without arguments (the ELF
//! loader gives none), passes `job /data/ps5upload/convert`.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str =
    "usage: ps5upload-convert inspect <path>\n       ps5upload-convert job <folder>";

// The PS5 libc serializes every malloc across threads (alloc.rs).
#[cfg(target_env = "ps5")]
#[global_allocator]
static ALLOC: ps5upload_convert::alloc::Pool = ps5upload_convert::alloc::Pool;

// The PS5 kernel fills FreeBSD 11 structs; libc's default FreeBSD 12 layouts read every field at
// the wrong offset (PS5 Dump Forge found on hardware that every path then looks like neither a
// file nor a folder). payload/convert/build.sh selects the FreeBSD 11 ABI; these fail the build
// if that is ever lost. The checks are Forge's (crates/ps5-dump-forge-cli/src/main.rs).
#[cfg(target_env = "ps5")]
const _: () = {
    assert!(std::mem::size_of::<libc::stat>() == 120);
    assert!(std::mem::size_of::<libc::dirent>() == 264);
    // MNAMELEN 88 (FreeBSD 12+: 1024): free space, mount points, filesystem types.
    assert!(std::mem::size_of::<libc::statfs>() == 472);
};

fn main() -> ExitCode {
    // rayon's global pool (the BLAKE3 hashing) to the job's share of the console's CPUs.
    #[cfg(target_env = "ps5")]
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(ps5upload_convert::job::THREADS)
        .build_global();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["inspect", path] => match ps5upload_convert::inspect(Path::new(path)) {
            Ok(inspection) => {
                // A closed stdout (a pipe's reader gone) is not worth a panic.
                let _ = writeln!(std::io::stdout(), "{}", serde_json::json!(inspection));
                ExitCode::SUCCESS
            }
            Err(refusal) => {
                ps5upload_convert::log(format_args!("ps5upload-convert: {refusal}"));
                ExitCode::FAILURE
            }
        },
        ["job", dir] => match ps5upload_convert::job::run(Path::new(dir)) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(_) => ExitCode::FAILURE,
            Err(e) => {
                ps5upload_convert::log(format_args!(
                    "ps5upload-convert: cannot write the status in {dir}: {e}"
                ));
                ExitCode::from(2)
            }
        },
        _ => {
            ps5upload_convert::log(format_args!("{USAGE}"));
            ExitCode::from(2)
        }
    }
}
