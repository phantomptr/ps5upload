/* json_find_key: a registered folder game's en-US title, never the block after
 * "defaultLanguage": "en-US" (register.c parse_title_from_param_json). */
#include <stdio.h>
#include <string.h>

#include "json_key.h"

static int fails;
#define CHECK(c) do { if (!(c)) { fprintf(stderr, "FAIL %s:%d %s\n", __FILE__, __LINE__, #c); fails++; } } while (0)

int main(void) {
    /* Keys sorted, as the converter writes them (a BTreeMap). */
    const char *sorted =
        "{\"localizedParameters\":{\"de-DE\":{\"titleName\":\"Spiel\"},"
        "\"defaultLanguage\": \"en-US\","
        "\"el-GR\":{\"titleName\":\"Paichnidi\"},"
        "\"en-GB\":{\"titleName\":\"Game GB\"},"
        "\"en-US\" :\n {\"titleName\":\"Game\"}},\"titleId\":\"PPSA01234\"}";
    const char *k = json_find_key(sorted, "\"en-US\"");
    CHECK(k != NULL);
    CHECK(k && strncmp(k, "\"en-US\" :", 9) == 0);
    CHECK(k && strstr(k, "\"titleName\":\"Game\"") == strstr(k, "\"titleName\""));
    /* Only a value: no key. */
    CHECK(json_find_key("{\"defaultLanguage\":\"en-US\"}", "\"en-US\"") == NULL);
    CHECK(json_find_key("{\"a\":1}", "\"en-US\"") == NULL);
    CHECK(json_find_key("{\"en-US\"", "\"en-US\"") == NULL); /* cut short: no ':' */
    CHECK(json_find_key("{}", "") == NULL);
    printf("json_key_selftest: %s\n", fails ? "FAILED" : "all passed");
    return fails ? 1 : 0;
}
