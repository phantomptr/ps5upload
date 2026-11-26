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

/* A paired-form value in a pretty-printed SHN: the whitespace around it is not part of it, and one
 * too long for the buffer is reported cut and fills it (cheats.c then refuses "value too long"). It
 * used to be cut first and trimmed after, so a cut value passed for a whole one. */
static void xml_text_measures_without_whitespace(void) {
    char out[256], small[32];
    CHECK(cheats_xml_text("<Cheatline><ValueOn>\n      90 90 \n   </ValueOn></Cheatline>", "ValueOn", out,
                          sizeof out) == 0);
    CHECK(strcmp(out, "90 90") == 0);
    CHECK(cheats_xml_text("<Cheatline><Offset a=\"1\">0x10</Offset>", "Offset", small, sizeof small) == 0);
    CHECK(strcmp(small, "0x10") == 0);
    CHECK(cheats_xml_text("<Cheatline><ValueOn>90</Cheatline>", "ValueOn", out, sizeof out) == -1);
    CHECK(out[0] == '\0');
    CHECK(cheats_xml_text("<Cheatline/>", "ValueOn", out, sizeof out) == -1);

    /* 20 spaces of indentation, then 300 hex digits. */
    char *xml = malloc(512);
    CHECK(xml != NULL);
    if (!xml) return;
    size_t at = (size_t)snprintf(xml, 512, "<ValueOn>\n");
    memset(xml + at, ' ', 20);
    at += 20;
    memset(xml + at, 'A', 300);
    at += 300;
    snprintf(xml + at, 512 - at, "\n</ValueOn>");
    CHECK(cheats_xml_text(xml, "ValueOn", out, sizeof out) == 1);
    CHECK(strlen(out) + 1 == sizeof out); /* full: the caller's "too long" check trips */
    CHECK(out[0] == 'A');
    /* Exactly what fits, indented: whole. */
    at = (size_t)snprintf(xml, 512, "<ValueOn>\n");
    memset(xml + at, ' ', 20);
    at += 20;
    memset(xml + at, 'B', 254);
    at += 254;
    snprintf(xml + at, 512 - at, "\n</ValueOn>");
    CHECK(cheats_xml_text(xml, "ValueOn", out, sizeof out) == 0);
    CHECK(strlen(out) == 254 && out[0] == 'B' && out[253] == 'B');
    free(xml);
}

/* Each <Cheatline> / <Cheat> ends where it does, self-closing or paired, in whatever mix: walked
 * the way parse_shn_file walks a file. */
static void shn_elements_end_at_their_own_tags(void) {
    const char *cheat =
        "<Cheatline Offset=\"10\" ValueOn=\"01\" ValueOff=\"00\"/>"
        "<Cheatline><Offset>20</Offset><ValueOn>02</ValueOn><ValueOff>00</ValueOff></Cheatline>"
        "<Cheatline Offset=\"30\" ValueOn=\"03\" ValueOff=\"00\"/>";
    const char *want[] = {"10", "20", "30"};
    const char *cur = cheat;
    int n = 0;
    for (;;) {
        const char *s = strstr(cur, "<Cheatline");
        if (!s) break;
        const char *e = strchr(s, '>') + 1, *body;
        const char *after = cheats_xml_element_end(s, e, "</Cheatline>", &body);
        CHECK(after != NULL);
        if (!after || n >= 3) break;
        char line[256], off[16] = "";
        size_t len = (size_t)(after - s);
        memcpy(line, s, len);
        line[len] = '\0';
        if (cheats_xml_attr(line, "Offset", off, sizeof off) != 0)
            CHECK(cheats_xml_text(line, "Offset", off, sizeof off) == 0);
        CHECK(strcmp(off, want[n]) == 0);
        n++;
        cur = after;
    }
    CHECK(n == 3); /* the paired line after a self-closing one is not skipped */

    /* <Cheat .../> has no lines: the next cheat's lines and close stay the next cheat's. */
    const char *file = "<Cheat Text=\"A\"/><Cheat Text=\"B\"><Cheatline Offset=\"40\"/></Cheat>";
    const char *s = strstr(file, "<Cheat");
    const char *e = strchr(s, '>') + 1, *body;
    const char *after = cheats_xml_element_end(s, e, "</Cheat>", &body);
    CHECK(after == e && body == e);
    CHECK(after && strncmp(after, "<Cheat Text=\"B\">", 16) == 0);
    /* A paired element with no close at all. */
    const char *open = "<Cheat Text=\"C\"><Cheatline Offset=\"1\"/>";
    CHECK(cheats_xml_element_end(open, strchr(open, '>') + 1, "</Cheat>", &body) == NULL);
}

int main(void) {
    shn_elements_end_at_their_own_tags();
    xml_attr_finds_whole_names();
    xml_attr_stops_at_the_end();
    xml_text_measures_without_whitespace();
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
