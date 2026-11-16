#ifndef PS5UPLOAD_INDICATOR_H
#define PS5UPLOAD_INDICATOR_H

/* The console's beeper, as a shell built-in:
 *
 *   beep [0-3]        sound the beeper once (pattern 0-3, default 1)
 *
 * The front light is not controlled: /dev/icc_indicator's enable flag and
 * sceKernelIccSetDynamicLedDimSetting both returned 0 on a PS5 and changed nothing (#840).
 *
 * Returns 1 when `argv[0]` was one of ours (with *out_text malloc'd and *out_exit set),
 * 0 when it was not. */
int indicator_shell(int argc, char *argv[], char **out_text, int *out_exit);

/* The same for the management channel. Returns 0 on success, the console's own error code, or
 * INDICATOR_UNAVAILABLE when the call cannot be resolved on this firmware. An out-of-range
 * pattern is refused (INDICATOR_BAD_VALUE). */
#define INDICATOR_UNAVAILABLE (-1000)
#define INDICATOR_BAD_VALUE   (-1001)
int indicator_beep(int pattern); /* 0-3 */

#endif
