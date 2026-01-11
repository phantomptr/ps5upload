/* PS5 Upload Server Configuration */

#ifndef CONFIG_H
#define CONFIG_H

// Network settings
#define SERVER_PORT 9113
#define MAX_CONNECTIONS 8
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

// Debug
#define DEBUG_LOG 0

#endif /* CONFIG_H */
