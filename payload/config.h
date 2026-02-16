/* PS5 Upload Server Configuration */

#ifndef CONFIG_H
#define CONFIG_H

// Network settings
#define SERVER_PORT 9113
#define MAX_CONNECTIONS 32
#define RECV_TIMEOUT_SEC 300  // 5 minutes
#define IDLE_TIMEOUT_SEC 0     // 0 disables idle auto-exit

// Buffer sizes - optimized for high throughput
#define BUFFER_SIZE (4 * 1024 * 1024)  // 4MB transfer buffer
#define CMD_BUFFER_SIZE 4096
#define MAX_COMMAND_DISPATCH_THREADS 64

// Socket buffer tuning
// PS5/FreeBSD 11 has tighter kernel socket buffer and process budget constraints than a desktop.
// Keep defaults conservative to avoid kernel memory pressure. Transfer paths can still bump these.
#define SOCKET_RCVBUF_SIZE (1 * 1024 * 1024)   // 1MB socket receive buffer (stability default)
#define SOCKET_SNDBUF_SIZE (1 * 1024 * 1024)   // 1MB socket send buffer (stability default)

// Upload throughput tuning
#define UPLOAD_RCVBUF_SIZE (2 * 1024 * 1024)   // 2MB receive buffer for bulk upload sockets
#define UPLOAD_RECV_CHUNK_SIZE (256 * 1024)    // 256KB recv chunks (keeps latency/pressure reasonable)
#define MAX_PATH_LEN 4096

// Backpressure tuning - avoid busy spinning
#define BACKPRESSURE_POLL_US 100  // 100us between backpressure checks (was 1000us)

// Thread stack size - ensure enough stack for worker threads
#define THREAD_STACK_SIZE (512 * 1024)  // 512KB stack per thread

// Fast transfer (V4) resource tuning
// These defaults trade a small amount of peak throughput for much higher stability under PS5/FreeBSD 11
// process/resource budgets (thread + memory + socket buffer pressure).
#define FTX_PACK_BUFFER_SIZE (48 * 1024 * 1024)     // Keep in sync with desktop max pack size
#define FTX_PACK_QUEUE_DEPTH 2                      // Caps in-flight packs per writer queue
#define FTX_WRITER_THREAD_COUNT 3                   // Fewer threads reduces memory + context switching
#define FTX_POOL_SIZE 4                             // Pack buffer pool (each is FTX_PACK_BUFFER_SIZE)
#define FTX_FILE_WRITE_QUEUE_DEPTH 2048             // Large static queue costs memory; keep bounded
#define FTX_FILE_WRITER_THREAD_COUNT 2               // Parallel file writer threads for small-file I/O overlap
#define FTX_FILE_WRITER_BATCH_SIZE 512              // Heap batch size inside writer thread
#define FTX_SMALL_FILE_POOL_BUF_SIZE (256 * 1024)
#define FTX_SMALL_FILE_POOL_SIZE 32

// Default paths
#define DEFAULT_GAMES_PATH "/mnt/usb0/games"
#define FALLBACK_PATH "/data/games"

// Notification settings
#define ENABLE_NOTIFICATIONS 1
#define NOTIFY_PROGRESS_INTERVAL (1024 * 1024 * 1024)  // Every 1GB

// RAR extraction tuning
#define UNRAR_FAST_SLEEP_EVERY_BYTES (8ULL * 1024 * 1024)
#define UNRAR_FAST_SLEEP_US 1000
#define UNRAR_FAST_KEEPALIVE_SEC 10
#define UNRAR_FAST_PROGRESS_INTERVAL_SEC 10
#define UNRAR_FAST_TRUST_PATHS 0
#define UNRAR_FAST_PROGRESS_FILE_START 1

#define UNRAR_TURBO_SLEEP_EVERY_BYTES (32ULL * 1024 * 1024)
#define UNRAR_TURBO_SLEEP_US 0
#define UNRAR_TURBO_KEEPALIVE_SEC 20
#define UNRAR_TURBO_PROGRESS_INTERVAL_SEC 15
#define UNRAR_TURBO_TRUST_PATHS 1
#define UNRAR_TURBO_PROGRESS_FILE_START 0

#define UNRAR_SAFE_SLEEP_EVERY_BYTES (1ULL * 1024 * 1024)
#define UNRAR_SAFE_SLEEP_US 1000
#define UNRAR_SAFE_KEEPALIVE_SEC 5
#define UNRAR_SAFE_PROGRESS_INTERVAL_SEC 5
#define UNRAR_SAFE_TRUST_PATHS 0
#define UNRAR_SAFE_PROGRESS_FILE_START 1

// Copy/move progress updates
#define COPY_PROGRESS_INTERVAL_SEC 10
#define COPY_PROGRESS_BYTES (4ULL * 1024 * 1024)

// Debug
#define DEBUG_LOG 0

#endif /* CONFIG_H */
