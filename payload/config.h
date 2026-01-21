/* PS5 Upload Server Configuration */

#ifndef CONFIG_H
#define CONFIG_H

// Network settings
#define SERVER_PORT 9113
#define MAX_CONNECTIONS 12
#define RECV_TIMEOUT_SEC 300  // 5 minutes

// Buffer sizes
#define BUFFER_SIZE (1024 * 1024)  // 1MB transfer buffer
#define CMD_BUFFER_SIZE 4096
#define MAX_PATH_LEN 4096

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
#define UNRAR_FAST_PROGRESS_INTERVAL_SEC 5

#define UNRAR_TURBO_SLEEP_EVERY_BYTES (32ULL * 1024 * 1024)
#define UNRAR_TURBO_SLEEP_US 0
#define UNRAR_TURBO_KEEPALIVE_SEC 10
#define UNRAR_TURBO_PROGRESS_INTERVAL_SEC 5

#define UNRAR_SAFE_SLEEP_EVERY_BYTES (1ULL * 1024 * 1024)
#define UNRAR_SAFE_SLEEP_US 1000
#define UNRAR_SAFE_KEEPALIVE_SEC 5
#define UNRAR_SAFE_PROGRESS_INTERVAL_SEC 5

// Copy/move progress updates
#define COPY_PROGRESS_INTERVAL_SEC 10
#define COPY_PROGRESS_BYTES (4ULL * 1024 * 1024)

// Debug
#define DEBUG_LOG 0

#endif /* CONFIG_H */
