/* Tiny test harness for the tailnet unit tests. */
#ifndef TT_H
#define TT_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int tt_fail, tt_pass;

#define CHECK(cond)                                                                  \
	do {                                                                         \
		if (cond) {                                                          \
			tt_pass++;                                                   \
		} else {                                                             \
			tt_fail++;                                                   \
			fprintf(stderr, "%s:%d: FAIL: %s\n", __FILE__, __LINE__, #cond); \
		}                                                                    \
	} while (0)

#define CHECK_MEM(a, b, n) CHECK(memcmp((a), (b), (n)) == 0)

static inline size_t tt_unhex(unsigned char *out, size_t cap, const char *hex)
{
	size_t n = 0;
	int hi = -1;
	for (; *hex; hex++) {
		int v;
		char c = *hex;
		if (c >= '0' && c <= '9') v = c - '0';
		else if (c >= 'a' && c <= 'f') v = c - 'a' + 10;
		else if (c >= 'A' && c <= 'F') v = c - 'A' + 10;
		else continue; /* spaces, newlines */
		if (hi < 0) {
			hi = v;
		} else {
			if (n >= cap) {
				fprintf(stderr, "tt_unhex: overflow\n");
				exit(2);
			}
			out[n++] = (unsigned char)(hi << 4 | v);
			hi = -1;
		}
	}
	return n;
}

static inline int tt_done(const char *name)
{
	printf("%s: %d passed, %d failed\n", name, tt_pass, tt_fail);
	return tt_fail ? 1 : 0;
}

#endif
