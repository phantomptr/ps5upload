#ifndef PS5UPLOAD_INDICATOR_H
#define PS5UPLOAD_INDICATOR_H

/* The console's front LED and beeper, as two shell built-ins:
 *
 *   beep [0-3]        sound the beeper once (pattern 0-3, default 1)
 *   led off | on      turn the front LED off, or hand it back to the console
 *   led dim <0-2>     LED brightness (the console keeps it across restarts)
 *
 * Only the scalar controls are offered. The LED's colours live in a 130-byte per-channel
 * animation program (ioctl 0x40829505) whose layout on FW 13.60 is not understood, and
 * nothing here writes a structure it cannot read back and explain.
 *
 * Returns 1 when `argv[0]` was one of ours (with *out_text malloc'd and *out_exit set),
 * 0 when it was not. */
int indicator_shell(int argc, char *argv[], char **out_text, int *out_exit);

/* The same three controls for the management channel. Each returns 0 on success, a negative
 * errno, the console's own error code, or INDICATOR_UNAVAILABLE when the call cannot be
 * resolved on this firmware. Out-of-range values are refused (INDICATOR_BAD_VALUE). */
#define INDICATOR_UNAVAILABLE (-1000)
#define INDICATOR_BAD_VALUE   (-1001)
int indicator_beep(int pattern);  /* 0-3 */
int indicator_led(int on);        /* 0 = off, 1 = back to the console */
int indicator_led_dim(int level); /* 0-2 */

#endif
