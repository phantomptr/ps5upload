/* The USB adapter daemon's log (USBNET_LOG_PATH, kept under 256 KiB with one .old copy). Each line
 * starts with the seconds since the daemon started. MAC addresses are never written. */
#ifndef USBN_LOG_H
#define USBN_LOG_H

void usbn_log_open(void);
void say(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
/* Flushes the log to disk: before a step that could hang or take the console down. */
void checkpoint(void);

#endif
