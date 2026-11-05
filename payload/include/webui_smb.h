/* The console page's network-share routes (webui_api.c's table lists them). Saved shares live in
 * the helper (smb_shares.h); everything that talks to a server goes to the SMB daemon
 * (smb_client.h), started on first use.
 *
 *   /api/smb/shares                        -> {"shares":[share...]}   (no password, ever: has_password)
 *   /api/smb/discover {server,user,password,domain}
 *                                          -> {"host","port","share","path","shares":[{"name","remark"}]}
 *   /api/smb/save {id?,name,server,share,user,password?,domain,start_path}
 *       signs in and opens start_path first; a missing "password" with an id keeps the saved one
 *                                          -> {"share":share,"dialect"}
 *   /api/smb/remove {id}                   -> {}
 *   /api/smb/list {path,offset,limit}      -> {"entries":[{"name","kind","size","mtime"}],"more","next"}
 *   /api/smb/stat {path,missing_ok}        -> {"kind","size","mtime"} | {"missing":true}
 *   /api/smb/mkdir {path}                  -> {}
 *   /api/smb/copy {src,dest,overwrite}     -> {"job"}  one side "smb://<id>/<path>", the other a
 *       console path (checked against the helper's own read / write policy)
 *   /api/smb/delete {path}                 -> {"job"}  a file or folder tree on a share
 *   /api/smb/rename {from,to}              -> {}  inside one folder, never over something (a Replace's swap)
 *   /api/smb/job {job} | /api/smb/cancel {job} | /api/smb/jobs
 * share = {"id","name","host","port","share","user","domain","start_path","has_password","path"}.
 * Errors: {"error":token}; a server that refused the sign-in is 403 smb_auth (never 401: the page
 * reads 401 as "reload, the helper restarted"). */
#ifndef PS5UPLOAD_WEBUI_SMB_H
#define PS5UPLOAD_WEBUI_SMB_H

#include <stddef.h>
#include <stdint.h>

void webui_smb_shares(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_discover(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_save(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_remove(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_list(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_stat(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_mkdir(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_copy(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_delete(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_rename(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_job(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_cancel(const char *b, size_t n, uint8_t *out, int fd);
void webui_smb_jobs(const char *b, size_t n, uint8_t *out, int fd);

/* The game library (#498): POST /api/library/scan {source} starts the daemon's scan of a saved share
 * (source = its id; the share's start_path is walked) or of the console's drives (source "console")
 * -> {"job"}, read and cancelled like a copy (/api/smb/job, /api/smb/cancel). The result is
 * SMBD_LIB_DIR/<source>.json and its covers (smb/smbd_library.h), read by the page with /api/fs/read. */
void webui_library_scan(const char *b, size_t n, uint8_t *out, int fd);

/* The PC app's side of the same saved shares (mgmt smb.shares_list / _save / _remove): the reply
 * JSON into out, its length. {"ok":true,"shares":[share...],"max"} | {"ok":true,"share":share,
 * "dialect"} (after the same sign-in check as /api/smb/save) | {"ok":true}; a refusal is
 * {"ok":false,"err":token[,"detail"]}. Never a password. */
int webui_smb_pc_list(char *out, size_t cap);
int webui_smb_pc_save(const char *b, size_t n, char *out, size_t cap);
int webui_smb_pc_remove(const char *b, size_t n, char *out, size_t cap);

/* The same for console.tasks and console.task_cancel: the daemon's job list as JSON text (malloc'd,
 * the caller frees it; NULL when there is no daemon or it does not answer soon; never starts one),
 * and stopping job `id` (32 lowercase hex). 0, or -1 with a token in err. */
char *webui_smb_pc_jobs(void);
int webui_smb_pc_cancel(const char *id, char *err, size_t err_cap);

/* The HTTP status for a daemon / route error token (pure, host-tested). */
int webui_smb_http_status(const char *token);

#endif
