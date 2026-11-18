/* Host tests for two pieces of cheats.c that read a cheat file's own text: the XML attribute
 * reader (cheats_xml_attr.h) and the buffer a title's cheat list is built in (cheats_jbuf.h).
 * Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "../src/cheats_list.c" /* cheats_utf8_char_len */
#include "cheats_jbuf.h"
#include "cheats_xml_attr.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static int attr_is(const char *tag, const char *attr, const char *want) {
    char out[64];
    if (cheats_xml_attr(tag, attr, out, sizeof out) != 0) return 0;
    return strcmp(out, want) == 0;
}

static int attr_absent(const char *tag, const char *attr) {
    char out[64] = "x";
    return cheats_xml_attr(tag, attr, out, sizeof out) == -1 && out[0] == '\0';
}

static void xml_attr_finds_whole_names(void) {
    const char *tag = "<Cheat Text=\"Inf HP\" ContextText=\"no\" Description = \"God mode\">";
    CHECK(attr_is(tag, "Text", "Inf HP"));
    CHECK(attr_is(tag, "Description", "God mode"));
    CHECK(attr_is("<Cheat ContextText=\"no\" Text=\"yes\">", "Text", "yes"));
    CHECK(attr_absent("<Cheat ContextText=\"no\">", "Text"));
    CHECK(attr_absent(tag, "Name"));
    CHECK(attr_absent(tag, ""));
    char small[4];
    CHECK(cheats_xml_attr(tag, "Text", small, sizeof small) == 0 && strcmp(small, "Inf") == 0);
}

/* A name without ="value" at the very end of the tag: the search must stop at the tag's NUL. The
 * buffer is reused per line in cheats.c, so what lies after the NUL is the PREVIOUS line's text;
 * the reader used to step over the NUL and return that line's value. */
static void xml_attr_stops_at_the_end(void) {
    char buf[128];
    memset(buf, 0, sizeof buf);
    /* The earlier, longer line. */
    snprintf(buf, sizeof buf, "<Cheatline Offset=\"1000\" ValueOn=\"90\" ValueOff=\"AABBCCDD\"/>");
    /* The next line, shorter, ends in a bare name; the old line's tail stays after its NUL. */
    const char *next = "<Cheatline Offset=\"2000\" ValueOn=\"EB\" ValueOff";
    memcpy(buf, next, strlen(next) + 1);
    CHECK(attr_is(buf, "Offset", "2000"));
    CHECK(attr_absent(buf, "ValueOff"));

    /* The same at the exact end of a heap block (ASan/valgrind see a read past it). */
    const char *cut = "<Cheat Description=\"d\" Text";
    size_t n = strlen(cut) + 1;
    char *exact = malloc(n);
    if (!exact) abort();
    memcpy(exact, cut, n);
    CHECK(attr_absent(exact, "Text"));
    CHECK(attr_is(exact, "Description", "d"));
    free(exact);

    /* A bare name in the middle does not hide a later, real one. */
    CHECK(attr_is("<C ValueOff ValueOff=\"11\">", "ValueOff", "11"));
    CHECK(attr_absent("<C ValueOff=11>", "ValueOff"));
}

/* One list entry the way cheats_list_mods writes it; 0 when it did not fit and was taken out. */
static int add_entry(jbuf_t *jb, int first, int index, const char *name, const char *hint, const char *type) {
    size_t mark = jb->off;
    if (!first) jb_raw(jb, ",");
    JB_PRINTF(jb, "{\"index\":%d,\"name\":\"", index);
    jb_str(jb, name);
    JB_PRINTF(jb, "\",\"desc\":\"");
    jb_str(jb, hint);
    jb_raw(jb, "\",\"type\":\"");
    jb_str(jb, type);
    JB_PRINTF(jb, "\",\"on\":%s", "false");
    jb_raw(jb, "}");
    if (jb->over) {
        jb_rollback(jb, mark);
        return 0;
    }
    return 1;
}

/* Brackets and braces balance outside strings, and every string closes. */
static int balanced(const char *s) {
    int depth = 0, in_str = 0;
    for (; *s; s++) {
        if (in_str) {
            if (*s == '\\' && s[1]) s++;
            else if (*s == '"') in_str = 0;
            else if ((unsigned char)*s < 0x20) return 0;
        } else if (*s == '"') in_str = 1;
        else if (*s == '{' || *s == '[') depth++;
        else if (*s == '}' || *s == ']') {
            if (--depth < 0) return 0;
        }
    }
    return depth == 0 && !in_str;
}

static void list_ends_whole_at_any_size(void) {
    char name[256], hint[256];
    memset(name, '"', sizeof name - 1); /* every byte doubles when escaped */
    name[sizeof name - 1] = 0;
    memset(hint, 'h', sizeof hint - 1);
    hint[sizeof hint - 1] = 0;
    for (size_t cap = 300; cap <= 6000; cap += 7) {
        char *buf = malloc(cap);
        if (!buf) abort();
        memset(buf, 0x55, cap);
        jbuf_t jb;
        int truncated = 0, n = 0;
        jb_init(&jb, buf, cap - CHEATS_MODS_TAIL_ROOM);
        jb_raw(&jb, "{\"title_id\":\"");
        jb_str(&jb, "CUSA00001");
        jb_raw(&jb, "\",\"mods\":[");
        for (int i = 0; i < 12; i++) {
            if (!add_entry(&jb, n == 0, i, name, hint, "checkbox")) {
                truncated = 1;
                break;
            }
            n++;
        }
        jb.cap = cap;
        jb_raw(&jb, truncated ? "],\"truncated\":true}" : "]}");
        CHECK(!jb.over);
        CHECK(jb.off < cap && buf[jb.off] == '\0' && strlen(buf) == jb.off);
        CHECK(balanced(buf));
        CHECK(strstr(buf, "\"mods\":[") != NULL);
        /* Twelve such entries need ~10 KiB: every size here cuts the list, at a whole entry. */
        CHECK(truncated && strstr(buf, "],\"truncated\":true}") != NULL);
        int seen = 0;
        for (const char *p = buf; (p = strstr(p, "{\"index\":")) != NULL; p++) seen++;
        CHECK(seen == n);
        free(buf);
    }
    /* Room for everything: no "truncated", as before. */
    char big[32 * 1024];
    jbuf_t jb;
    jb_init(&jb, big, sizeof big - CHEATS_MODS_TAIL_ROOM);
    jb_raw(&jb, "{\"mods\":[");
    for (int i = 0; i < 12; i++) CHECK(add_entry(&jb, i == 0, i, name, hint, "checkbox"));
    jb.cap = sizeof big;
    jb_raw(&jb, "]}");
    CHECK(!jb.over && balanced(big) && strstr(big, "truncated") == NULL);
}

/* The type is the file's text like the name: a quote, a control byte or a stray byte in it must
 * not break the reply (it was printed as it was). */
static void type_is_escaped(void) {
    char buf[512];
    jbuf_t jb;
    jb_init(&jb, buf, sizeof buf);
    jb_raw(&jb, "[");
    CHECK(add_entry(&jb, 1, 0, "n", "h", "a\"b\\c\td\xE9"));
    jb_raw(&jb, "]");
    CHECK(balanced(buf));
    CHECK(strstr(buf, "\"type\":\"a\\\"b\\\\cd?\"") != NULL);
}

static void flags_what_was_cut(void) {
    char buf[16];
    jbuf_t jb;
    jb_init(&jb, buf, sizeof buf);
    jb_raw(&jb, "0123456789");
    CHECK(!jb.over && jb.off == 10);
    size_t mark = jb.off;
    jb_str(&jb, "abcdefgh");
    CHECK(jb.over && jb.off < sizeof buf && buf[jb.off] == '\0');
    jb_rollback(&jb, mark);
    CHECK(!jb.over && strcmp(buf, "0123456789") == 0);
    jb_raw(&jb, "abcdefgh");
    CHECK(jb.over && strlen(buf) == sizeof buf - 1);
    jb_rollback(&jb, mark);
    jb_str(&jb, "abc");
    CHECK(!jb.over && strcmp(buf, "0123456789abc") == 0);
    /* A multi-byte character is never cut in half. */
    jb_rollback(&jb, mark);
    jb_str(&jb, "ab\xE3\x82\xB2\xE3\x82\xB2");
    CHECK(jb.over && strcmp(buf, "0123456789ab") == 0);
}

int main(void) {
    xml_attr_finds_whole_names();
    xml_attr_stops_at_the_end();
    list_ends_whole_at_any_size();
    type_is_escaped();
    flags_what_was_cut();
    if (failures) {
        fprintf(stderr, "cheats_reply_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_reply_selftest: ok\n");
    return 0;
}
