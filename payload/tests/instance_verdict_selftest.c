/* How the previous payload instance ended. SIGKILL cannot be caught, so the
 * only evidence an externally-killed instance leaves is an ownership record
 * it never got to unlink. */
#include "instance_verdict.h"

#include <stdio.h>
#include <string.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    const uint64_t BOOT = 1000000;
    const uint64_t AFTER_BOOT = BOOT + 500;
    const uint64_t BEFORE_BOOT = BOOT - 500;

    /* No record: the previous instance unlinked it on a graceful exit, or
     * this is the first run ever. */
    check(instance_verdict_classify(0, 0, BOOT, 0) == PS5UPLOAD2_PRIOR_CLEAN,
          "no record means clean exit");
    check(instance_verdict_classify(0, AFTER_BOOT, BOOT, 1) == PS5UPLOAD2_PRIOR_CLEAN,
          "no record wins over every other input");

    /* The signature we care about: a record from THIS boot whose pid is
     * gone. It died without unlinking — SIGKILL or OOM. */
    check(instance_verdict_classify(1, AFTER_BOOT, BOOT, 0)
              == PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY,
          "record from this boot + pid gone = killed externally");

    /* Still running: wedged, and the reap path deals with it. */
    check(instance_verdict_classify(1, AFTER_BOOT, BOOT, 1)
              == PS5UPLOAD2_PRIOR_WEDGED,
          "record from this boot + pid alive = wedged");

    /* The ownership file lives on persistent /data and survives reboots, so
     * a record predating this boot says nothing about this session. */
    check(instance_verdict_classify(1, BEFORE_BOOT, BOOT, 0)
              == PS5UPLOAD2_PRIOR_STALE,
          "record from a previous boot is stale");
    check(instance_verdict_classify(1, BEFORE_BOOT, BOOT, 1)
              == PS5UPLOAD2_PRIOR_STALE,
          "stale wins over pid-alive");

    /* Unknowable inputs must never be reported as a real verdict. */
    check(instance_verdict_classify(1, 0, BOOT, 0) == PS5UPLOAD2_PRIOR_STALE,
          "old-format record with no start time is stale");
    check(instance_verdict_classify(1, AFTER_BOOT, 0, 0) == PS5UPLOAD2_PRIOR_STALE,
          "unavailable boottime is stale");

    /* Names are the wire contract — snake_case, stable. */
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_CLEAN), "clean"),
          "clean name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY),
                  "killed_externally"),
          "killed_externally name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_WEDGED), "wedged"),
          "wedged name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_STALE), "stale"),
          "stale name");
    check(!strcmp(instance_verdict_name((ps5upload2_prior_verdict_t)99),
                  "unknown"),
          "out-of-range verdict has a name");

    printf("\ninstance_verdict_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
