/* Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the
 * Free Software Foundation; either version 3, or (at your option) any
 * later version.
 */

#include <stdio.h>
#include <string.h>

#include "notify.h"

typedef struct notify_request {
    char useless1[45];
    char message[3075];
} notify_request_t;

int sceKernelSendNotificationRequest(int, notify_request_t*, size_t, int);

static void send_notification(const char *message) {
    notify_request_t req;
    memset(&req, 0, sizeof(req));
    strncpy(req.message, message, sizeof(req.message) - 1);
    sceKernelSendNotificationRequest(0, &req, sizeof(req), 0);
}

void notify_info(const char *title, const char *message) {
    char full_msg[256];
    snprintf(full_msg, sizeof(full_msg), "%s: %s", title, message);
    send_notification(full_msg);
}

void notify_success(const char *title, const char *message) {
    char full_msg[256];
    snprintf(full_msg, sizeof(full_msg), "✓ %s: %s", title, message);
    send_notification(full_msg);
}

void notify_error(const char *title, const char *message) {
    char full_msg[256];
    snprintf(full_msg, sizeof(full_msg), "✗ %s: %s", title, message);
    send_notification(full_msg);
}
