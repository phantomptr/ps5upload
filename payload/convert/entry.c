/* ps5upload-convert's entry on the PS5, linked in front of Rust's main with -Wl,--wrap=main (the
 * arrangement of PS5 Dump Forge's ps5/entry.c; what it does before main is the extractor's,
 * payload/extractor/src/xmain.c).
 *
 * The helper starts this program by sending the ELF to the console's ELF loader, which gives a
 * payload no arguments and its own socket as stdio. So, before Rust runs:
 *   - the process takes a name of its own;
 *   - stdin becomes /dev/null and stdout/stderr the log in the job folder;
 *   - with no arguments, main gets "job <folder>": the request the helper wrote there is the work.
 * Started with arguments (a loader's args=, a developer's run), main gets them as they are.
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

#include "proc_identity.h"

/* The helper's folder for this program; payload/include names the same path for the helper side
 * once it starts conversions (the way EXTRACT_DIR is shared with the extractor). */
#define CONVERT_DIR "/data/ps5upload/convert"

#define CONVERT_PROC_NAME "ps5u-convert"

extern int __real_main(int argc, char **argv);

/* Rust names its worker threads ("zstd-pack-3", "image-hash"), and on the PS5 the last thread to
 * name itself names the whole process (proc_identity.h; measured on the helper, issue #593): the
 * console's process list would show the converter under a different name every few seconds. So
 * every thread that names itself takes the one process name instead (linker.sh:
 * --wrap=pthread_set_name_np). Naming another thread is dropped: nothing here does it. */
void __wrap_pthread_set_name_np(pthread_t thread, const char *name) {
    (void)name;
    if (pthread_equal(thread, pthread_self())) proc_name_set_self(CONVERT_PROC_NAME);
}

int __wrap_main(int argc, char **argv) {
    static char *job_args[] = {"ps5upload-convert", "job", CONVERT_DIR, NULL};
    int devnull, log, rc;

    /* Not "payload.elf" (the name elfldr gives raw payloads, which other payloads sweep for) and
     * not "ps5upload-*": the helper SIGKILLs every process of that name as a stray copy of itself
     * (runtime_sweep_our_instances). */
    proc_name_set_self(CONVERT_PROC_NAME);

    /* The helper makes the job folder (0700, as it makes the extractor's) before it starts this
     * program; these are for a start by hand, before any helper ran. */
    if (mkdir("/data/ps5upload", 0777) != 0 && errno != EEXIST) return 1;
    if (mkdir(CONVERT_DIR, 0700) != 0 && errno != EEXIST) return 1;

    /* The loader hands us its socket as stdio; the helper has long closed it, and a write to it
     * would end the program with SIGPIPE in the middle of a job. */
    devnull = open("/dev/null", O_RDONLY);
    log = open(CONVERT_DIR "/convert.log", O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, 0644);
    if (devnull >= 0) {
        dup2(devnull, STDIN_FILENO);
        if (devnull != STDIN_FILENO) close(devnull);
    }
    if (log < 0) return 1;
    dup2(log, STDOUT_FILENO);
    dup2(log, STDERR_FILENO);
    if (log > STDERR_FILENO) close(log);
    setvbuf(stdout, NULL, _IOLBF, 0);

    rc = argc > 1 ? __real_main(argc, argv) : __real_main(3, job_args);
    fflush(NULL);
    return rc;
}
