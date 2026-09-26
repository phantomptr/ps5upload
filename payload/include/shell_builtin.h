#ifndef PS5UPLOAD_SHELL_BUILTIN_H
#define PS5UPLOAD_SHELL_BUILTIN_H

#include <stddef.h>
#include <stdint.h>
#include <sys/mount.h>

/*
 * The payload's built-in shell.
 *
 * The PS5 has no /bin/sh, so this implements ~44 commands itself (ls,
 * cat, grep, find, df, ps, …). It lived inside runtime.c as a single
 * 1,981-line function in an 18,600-line file, which is most of why that
 * file was too large to audit — a duplicate app.db reader hid in there
 * for months.
 *
 * Split out because it is almost entirely self-contained: it needs
 * nothing from the FTX2 runtime except two helpers, declared below. The
 * frame plumbing (shell_send_json_result, handle_shell_exec) stays in
 * runtime.c, since that genuinely does need runtime_state_t.
 */

/* Provided by runtime.c — shared rather than duplicated. */
char *strdup_safe(const char *s);
int mntinfo_snapshot(struct statfs **out);

/* Session cwd tracking, keyed by the client's session id. */
const char *shell_valid_cwd(const char *cwd);
int shell_valid_session_id(const char *session_id);
void shell_session_get(const char *session_id, const char *fallback, char *out,
                       size_t out_size);
void shell_session_set(const char *session_id, const char *cwd);

/* Minimal JSON field extraction for the shell request body. */
int shell_json_string_field(const char *body, uint64_t body_len,
                            const char *field, char *out, size_t cap);

/* Helpers the FTX2 shell handler still needs for its fast paths. */
int shell_split(char *cmd, char *argv[], int max_args);
size_t shell_appendf(char **buf, size_t *cap, size_t len, const char *fmt, ...);
int shell_resolve_dir(const char *cwd, const char *path, char *out, size_t cap,
                      char *err, size_t err_cap);
int shell_ls_path(const char *path, char **out_text, int *out_exit);

/* Run a command with the session cwd applied, serialised against other
 * sessions because chdir() is process-global. */
int shell_run_in_cwd(const char *cwd, const char *cmd, char **out_text,
                     int *out_exit);

/* Run one command line. Allocates *out_text (caller frees) and sets
 * *out_exit to the command's exit status. Returns 0 when the command was
 * recognised, -1 when it was not. */
int handle_shell_builtin(const char *cmd_in, char **out_text, int *out_exit);

#endif
