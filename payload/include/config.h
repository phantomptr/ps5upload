#ifndef PS5UPLOAD2_CONFIG_H
#define PS5UPLOAD2_CONFIG_H

/* Compile-time version reported in the STATUS_ACK body. Lets the desktop
 * UI tell apart an old payload still running from a build that includes
 * a particular fix, without having to boot the console. Keep in sync
 * with the desktop app's package.json during releases. */
#define PS5UPLOAD2_VERSION "2.2.23"
/* Author credit — embedded in the startup toast so anyone looking at
 * the console screen knows who wrote the software that just loaded.
 * Kept separate from VERSION so release scripts can bump the version
 * without touching this line. */
#define PS5UPLOAD2_AUTHOR "PhantomPtr"

/* Transfer port — handles bulk-data frames (BEGIN_TX, STREAM_SHARD,
 * COMMIT_TX, ABORT_TX). Single-client at a time; a running transfer
 * blocks other callers on this port. */
#define PS5UPLOAD2_RUNTIME_PORT 9113
/* Management port — HELLO, STATUS, FS_LIST_DIR, CLEANUP, QUERY_TX,
 * TAKEOVER_REQUEST, and FS mutation frames. Served by a separate
 * pthread so it stays responsive during an active transfer. */
#define PS5UPLOAD2_MGMT_PORT 9114
#define PS5UPLOAD2_RUNTIME_ROOT "/data/ps5upload"
#define PS5UPLOAD2_RUNTIME_DIR "/data/ps5upload/runtime"
#define PS5UPLOAD2_TX_DIR "/data/ps5upload/tx"
#define PS5UPLOAD2_SPOOL_DIR "/data/ps5upload/spool"
#define PS5UPLOAD2_DEBUG_DIR "/data/ps5upload/debug"
/* Per-mount tracking files (name.src containing the source image
 * path). Used by FS_LIST_VOLUMES to surface which .exfat/.ffpkg
 * backs each /mnt/ps5upload/<name> mount point, and by payload
 * startup reconciliation to detect orphaned mounts. */
#define PS5UPLOAD2_MOUNTS_DIR "/data/ps5upload/mounts"

/* Socket tuning for the accepted client connection.
 * PS5/FreeBSD caps SO_RCVBUF at ~512 KiB per socket (kern.ipc.maxsockbuf
 * policy); asking for 8 MiB silently clamps. 512 KiB is enough for
 * ~800 Mbps on a 0.6 ms RTT LAN.
 * Note: listener's SO_RCVBUF is clamped harder (to ~64 KiB) on PS5, so the
 * per-socket setting happens on the *accepted* fd after each accept(2).
 */
#define PS5UPLOAD2_CLIENT_RCVBUF_BYTES (512 * 1024)
#define PS5UPLOAD2_CLIENT_SNDBUF_BYTES (512 * 1024)
/* Idle-socket read timeout (seconds). Stops a misbehaving client from
 * pinning the single-threaded server on a persistent connection forever.
 */
#define PS5UPLOAD2_CLIENT_IDLE_SEC 120

/* In-memory per-shard receive buffer size. Larger = fewer recv syscalls
 * and better disk-write batching. Matches the legacy payload's
 * `BUFFER_SIZE` (4 MiB) for the high-throughput bulk-upload path.
 */
#define PS5UPLOAD2_SHARD_IO_BUF (4 * 1024 * 1024)

/* Minimum shard size (payload bytes) for spawning the double-buffered writer
 * thread. Below this, the pthread_create/join cost — measured at ~4–6 ms per
 * shard on FreeBSD 11 / PS5 — dominates the write itself. For small shards
 * we fall through to the in-thread recv+write path (still POSIX `write(2)`,
 * same disk performance, just no pthread lifecycle cost).
 * Sized at 64 KiB: covers the bulk of small-file workloads (15k+ of the
 * 223k files in a real game dir are <64 KiB) while still using the writer
 * thread for 1 MiB+ shards where overlap earns its keep. */
#define PS5UPLOAD2_PIPED_THREAD_MIN_BYTES (64u * 1024u)

/* Packed-shard worker pool — parallelism for many-small-file workloads.
 * Every packed shard carries up to ~256 records (16 KiB files into 4 MiB
 * pack). Processing these serially on FreeBSD 11 UFS ran ~23 ms/file dominated
 * by directory-entry insertion and close-time metadata sync. A small pool of
 * real kernel threads lets those metadata operations overlap across files in
 * different directories (main gain), and lets open/close syscalls overlap
 * even in the same directory (smaller gain).
 *
 * Bounds are intentionally conservative — the PS5 has tight per-process
 * memory + thread limits, only one transaction is active on the single TCP
 * connection at a time, and the queue capacity bounds peak RAM per pool at
 * QUEUE_DEPTH * pack_file_max ≈ 4 MiB.
 */
/* Empirical note (2026-04-17 sweep): on PS5 UFS, increasing workers from 4 to
 * 8 gives zero wall-time improvement on 5000×16 KiB — PS5's filesystem
 * serializes file creation at a deep kernel level, capping real parallelism
 * at ~3.8× regardless of worker count. 4 workers is the Pareto-optimal
 * choice: saturates the kernel's per-file-create concurrency without wasting
 * stack memory or thread-context switches. */
#define PS5UPLOAD2_PACK_WORKERS 4u
#define PS5UPLOAD2_PACK_QUEUE_DEPTH 32u

/* Bounded retry budget for transient pack-worker open()/write() failures.
 * On the small-file-heavy regime (PPSA01342: 223k files / 19k dirs / 75k
 * shards) a single transient errno (EIO/EMFILE/ENOMEM) used to flip the
 * pool's sticky worker_error and abort the whole transaction — even a
 * 99.99% per-syscall success rate yielded ~50% chance of finishing. With 3
 * retries on transient errnos (backoff 20/50/100 ms) the same per-syscall
 * rate completes with effectively-1.0 probability. Terminal errnos
 * (ENOSPC/EROFS/EACCES/ENAMETOOLONG) bypass retry. */
#define PS5UPLOAD2_PACK_RETRY_MAX 3u

#endif
