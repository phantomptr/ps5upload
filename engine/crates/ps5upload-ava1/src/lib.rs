//! AVA1 behind `ps5upload-core`'s blocking transfer shapes. The engine's transfer
//! handlers (Task 23), the lab and the benchmark harness call the same `upload_*`
//! functions, so benchmarks exercise exactly what the app runs.
//!
//! The functions here are blocking: call them from `spawn_blocking` or a non-async
//! thread, exactly like the FTX2 functions they replace. Calling them from inside an
//! async task panics (`block_on` has tokio's "Cannot start a runtime from within a
//! runtime" behaviour — C15).

mod archive_time;
pub mod console;
pub mod copy;
pub mod download;
pub mod mgmt;
pub mod mgmt_convert;
pub mod mgmt_job;
pub mod pool;
pub mod progress;
#[cfg(not(target_os = "android"))]
pub mod rar_source;
pub mod relay;
pub mod seq;
pub mod source;
pub mod space;
pub mod telemetry;
pub mod upload;
pub mod zip_source;
mod zip_stored;
pub use zip_stored::StoredZipSink;

pub use pool::{pool, Pairing, Pool, JOURNAL_GC_EVERY, JOURNAL_MAX_AGE_S};
pub use upload::{PostCommitError, PostCommitKind};

/// Runs a future from blocking code (C15): a runtime handle's `block_on` when the
/// current thread belongs to a runtime (the engine's `spawn_blocking` workers — the
/// same pattern as `ps5upload_engine::convert_source`), or a lazily-built private
/// multi-thread runtime for callers outside any runtime (the lab's CLI, a unit test).
/// That fallback runtime is built once and lives for the process.
///
/// Calling this from inside an async task panics — tokio refuses to nest a runtime in
/// an async execution context — which is the same behaviour the blocking FTX2
/// functions have, and why the upload adapters must be called from `spawn_blocking`.
/// Inside a runtime, that runtime must be a multi-thread one (the engine's is).
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(f),
        Err(_) => fallback_runtime().block_on(f),
    }
}

fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("building a fallback tokio runtime")
    })
}
