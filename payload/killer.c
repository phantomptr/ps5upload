/* PS5 Upload Killer Payload */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <signal.h>

#include "notify.h"
#include "config.h"

#define PID_FILE "/data/ps5upload/payload.pid"
#define KILL_FILE "/data/ps5upload/kill.req"

static int request_shutdown(void) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock < 0) {
        return -1;
    }

    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(SERVER_PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(sock);
        return -1;
    }

    const char *cmd = "SHUTDOWN\n";
    send(sock, cmd, strlen(cmd), 0);

    struct timeval tv;
    tv.tv_sec = 2;
    tv.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char *)&tv, sizeof(tv));

    char buffer[64] = {0};
    int bytes = recv(sock, buffer, sizeof(buffer) - 1, 0);
    close(sock);

    if (bytes <= 0) {
        return -1;
    }

    buffer[bytes] = '\0';
    return (strncmp(buffer, "OK", 2) == 0) ? 0 : -1;
}

static int kill_by_pidfile(void) {
    FILE *fp = fopen(PID_FILE, "r");
    if (!fp) {
        return -1;
    }
    char buf[32] = {0};
    if (!fgets(buf, sizeof(buf), fp)) {
        fclose(fp);
        return -1;
    }
    fclose(fp);
    int pid = (int)strtol(buf, NULL, 10);
    if (pid <= 0) {
        return -1;
    }
    if (kill(pid, SIGTERM) != 0) {
        return -1;
    }
    usleep(200000);
    kill(pid, SIGKILL);
    unlink(PID_FILE);
    return 0;
}

static int request_kill_file(void) {
    FILE *fp = fopen(KILL_FILE, "w");
    if (!fp) {
        return -1;
    }
    fprintf(fp, "1\n");
    fclose(fp);
    return 0;
}

int main(void) {
    notify_info("PS5 Upload Killer", "Attempting to stop ps5upload...");

    if (request_shutdown() == 0) {
        notify_success("PS5 Upload Killer", "ps5upload stopped.");
        return 0;
    }

    request_kill_file();
    usleep(300000);
    if (request_shutdown() == 0) {
        notify_success("PS5 Upload Killer", "ps5upload stopped.");
        return 0;
    }

    if (kill_by_pidfile() == 0) {
        notify_success("PS5 Upload Killer", "ps5upload killed.");
        return 0;
    }

    notify_error("PS5 Upload Killer", "Failed to stop ps5upload.");
    return 1;
}
