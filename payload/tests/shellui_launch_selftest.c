/* Host-side test for how the ShellUI launch route reports what Sony's
 * launcher returned (shellui_launch_result, payload/include/shellui_rpc.h).
 *
 * The remote call hands back the function's whole return register. Sony's
 * error codes have bit 31 set (0x8094....), and the route used to return
 * `(int)rax`: a real refusal came out NEGATIVE. launch_title reads
 * `rc > 0` as "Sony refused" and anything negative other than -2 as "the
 * call never ran", so a refusal skipped the is-the-title-running check and
 * the single retry, fell into the in-process ladder (up to three more launch
 * requests — a second request at a starting title makes the shell background
 * it) and was reported as launch_all_strategies_failed with the code lost. */
#include <stdint.h>
#include <stdio.h>

#include "../include/shellui_rpc.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

int main(void) {
    uint32_t code = 0xdeadbeefu;

    /* Accepted. */
    CHECK(shellui_launch_result(0, 1, 0, 0, &code) == 0);
    CHECK(code == 0);

    /* THE REGRESSION: a Sony error, as the register holds it (an int return
     * zero-extended into rax). It must come out POSITIVE, code intact. */
    code = 0;
    CHECK(shellui_launch_result((long)0x80940005ul, 1, 0, 0, &code) ==
          SHELLUI_LAUNCH_SONY_CODE);
    CHECK(SHELLUI_LAUNCH_SONY_CODE > 0);
    CHECK(code == 0x80940005u);
    /* What the old `return (int)rax` produced for the same value. */
    CHECK((int)(long)0x80940005ul < 0);

    /* The same code sign-extended (a callee that widened its int). */
    code = 0;
    CHECK(shellui_launch_result((long)0xFFFFFFFF8094000Ful, 1, 0, 0, &code) ==
          SHELLUI_LAUNCH_SONY_CODE);
    CHECK(code == 0x8094000Fu);

    /* A small non-zero return keeps the branch it always took. */
    code = 0;
    CHECK(shellui_launch_result(5, 1, 0, 0, &code) == SHELLUI_LAUNCH_SONY_CODE);
    CHECK(code == 5u);

    /* An int -1 from the function itself (rax = 0x00000000FFFFFFFF) is a
     * returned value, not a ptrace failure: the call ran. */
    code = 0;
    CHECK(shellui_launch_result((long)0xFFFFFFFFul, 1, 0, 0, &code) ==
          SHELLUI_LAUNCH_SONY_CODE);
    CHECK(code == 0xFFFFFFFFu);

    /* Only the low 32 bits are the function's answer. */
    code = 0xdeadbeefu;
    CHECK(shellui_launch_result((long)0x7777777700000000ul, 1, 0, 0, &code) == 0);
    CHECK(code == 0);

    /* pt_call failed (-1), three ways; the meanings of -1 and -2 are kept. */
    code = 0xdeadbeefu;
    CHECK(shellui_launch_result(-1, 0, 0, 0, &code) == -1); /* never dispatched */
    CHECK(code == 0);
    CHECK(shellui_launch_result(-1, 1, 0, 0, &code) == -2); /* ran, cleanup raced */
    CHECK(shellui_launch_result(-1, 1, 1, 0, &code) == -1); /* timed out */
    CHECK(shellui_launch_result(-1, 1, 0, 1, &code) == -1); /* tracee lost */
    CHECK(shellui_launch_result(-1, 1, 1, 1, &code) == -1);

    /* The code pointer is optional. */
    CHECK(shellui_launch_result((long)0x80940005ul, 1, 0, 0, NULL) ==
          SHELLUI_LAUNCH_SONY_CODE);
    CHECK(shellui_launch_result(0, 1, 0, 0, NULL) == 0);

    printf("shellui_launch_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
