/* Host-side test for which in-process launch refusals end launch_title
 * (launch_refusal_is_final, payload/include/launch_route.h).
 *
 * The regression: on a Pro (FW 13.60) an installed fake package was refused
 * with 0x80020016, the ladder went on to the ShellUI ptrace route, and
 * SceShellUI restarted, taking ShadowMount+ down with it. A refusal the kernel
 * or the launcher gives for the title itself must stop there; the cases the
 * ShellUI route was written for must still reach it. */
#include <stdint.h>
#include <stdio.h>

#include "../include/launch_route.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

static int sce(uint32_t code) { return (int)code; }

int main(void) {
    /* THE INCIDENT: kernel EINVAL for a known title stops. */
    CHECK(launch_refusal_is_final(sce(0x80020016u)) == 1);
    /* Any SCE kernel error does: the kernel answers the same to ShellUI. */
    CHECK(launch_refusal_is_final(sce(0x80020001u)) == 1); /* EPERM */
    CHECK(launch_refusal_is_final(sce(0x8002000Du)) == 1); /* EACCES */
    CHECK(launch_refusal_is_final(sce(0x80020002u)) == 1); /* ENOENT */
    CHECK(launch_refusal_is_final(sce(0x8002FFFFu)) == 1);
    /* The homebrew/fake-package refusal: ShellUI dispatched and started nothing. */
    CHECK(launch_refusal_is_final(sce(0x80940033u)) == 1);

    /* Success: sceSystemServiceLaunchApp returns the new app id. */
    CHECK(launch_refusal_is_final(0) == 0);
    CHECK(launch_refusal_is_final(0xe018) == 0);
    /* Symbol absent on this firmware: the ShellUI route is the only one left. */
    CHECK(launch_refusal_is_final(-1) == 0);
    /* Launcher codes that keep the ladder (transient or caller-specific). */
    CHECK(launch_refusal_is_final(sce(0x8094000Fu)) == 0); /* no fg user yet */
    CHECK(launch_refusal_is_final(sce(0x80940005u)) == 0);
    CHECK(launch_refusal_is_final(sce(0x80940010u)) == 0);
    CHECK(launch_refusal_is_final(sce(0x8094000Cu)) == 0);
    CHECK(launch_refusal_is_final(sce(0x80940032u)) == 0);
    CHECK(launch_refusal_is_final(sce(0x80940034u)) == 0);
    /* Neighbouring namespaces are not kernel errors. */
    CHECK(launch_refusal_is_final(sce(0x80030016u)) == 0);
    CHECK(launch_refusal_is_final(sce(0x80010016u)) == 0);
    CHECK(launch_refusal_is_final(-2) == 0);

    printf("launch_route_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
