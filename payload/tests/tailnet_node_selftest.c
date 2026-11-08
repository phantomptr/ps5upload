/* tailnet_node.c on the host: the tailnet call's ops, settings on disk, start/stop/logout
 * and the status JSON. The control server here is a closed local port, so the node runs
 * and retries; the protocol and the forwarder are tested in tailnet/ (make -C tailnet test,
 * tailnet/testbed/m2-check.sh and m3-check.sh). Built with ASan + UBSan. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "tailnet_node.h"
#include "webui_codec.h"

static int fails, passes;
#define CHECK(c)                                                                   \
    do {                                                                           \
        if (c) passes++;                                                           \
        else {                                                                     \
            fails++;                                                               \
            fprintf(stderr, "%s:%d: FAIL: %s\n", __FILE__, __LINE__, #c);          \
        }                                                                          \
    } while (0)

static char out[65536];

static int call(const char *body) {
    int n = tailnet_node_call(body, strlen(body), out, sizeof out);
    if (n >= 0) out[n] = 0;
    return n;
}

static int str(const char *key, char *v, size_t cap) { return webui_json_str(out, strlen(out), key, v, cap); }
static int boolean(const char *key) {
    int b = -1;
    webui_json_bool(out, strlen(out), key, &b);
    return b;
}

static int file_has(const char *path, const char *needle) {
    char buf[2048];
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    return strstr(buf, needle) != NULL;
}

int main(void) {
    char dir[] = "/tmp/tailnet-node-selftest-XXXXXX", v[256], path[512];
    if (!mkdtemp(dir)) return 2;
    tailnet_node_set_dir(dir);

    /* nothing saved: off, the default control server */
    CHECK(call("") > 0);
    CHECK(boolean("ok") == 1 && boolean("enabled") == 0);
    CHECK(str("state", v, sizeof v) == 1 && !strcmp(v, "off"));
    CHECK(str("control_url", v, sizeof v) == 1 && !strcmp(v, TAILNET_DEFAULT_CONTROL));
    CHECK(tailnet_node_boot() == 0); /* disabled: does nothing */

    /* refused requests */
    CHECK(call("{\"op\":\"reboot\"}") > 0 && strstr(out, "unknown_op"));
    CHECK(call("{\"op\":\"enable\",\"control_url\":\"ftp://x\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":7}") > 0 && strstr(out, "bad_request"));

    /* enable against a closed port: the node runs and retries; settings are saved */
    CHECK(call("{\"op\":\"enable\",\"control_url\":\"http://127.0.0.1:9\",\"hostname\":\"Living room \\\"PS5\\\"\","
               "\"control_addr\":\"127.0.0.1\",\"authkey\":\"tskey-test\"}") > 0);
    CHECK(boolean("ok") == 1 && boolean("enabled") == 1);
    CHECK(str("state", v, sizeof v) == 1 && strcmp(v, "off") != 0);
    CHECK(str("hostname", v, sizeof v) == 1 && !strcmp(v, "Living room \"PS5\""));
    snprintf(path, sizeof path, "%s/settings.json", dir);
    CHECK(file_has(path, "\"enabled\":true") && file_has(path, "http://127.0.0.1:9"));
    CHECK(!file_has(path, "tskey")); /* the auth key is used once, never stored */
    snprintf(path, sizeof path, "%s/tailnet-state.json", dir);
    CHECK(access(path, F_OK) == 0); /* keys created on start */
    usleep(300 * 1000);
    CHECK(call("{\"op\":\"status\"}") > 0 && boolean("ok") == 1);
    CHECK(strstr(out, "\"peers\":[]") && strstr(out, "\"serving\":{\"udp_flows\":0,\"tcp_conns\":0}"));

    /* disable keeps the keys; boot with enabled=false stays off */
    CHECK(call("{\"op\":\"disable\"}") > 0 && boolean("enabled") == 0);
    CHECK(str("state", v, sizeof v) == 1 && !strcmp(v, "off"));
    CHECK(access(path, F_OK) == 0);

    /* boot after a restart with enabled=true starts it again (settings re-read) */
    CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
    tailnet_node_shutdown();
    tailnet_node_set_dir(dir);
    CHECK(tailnet_node_boot() == 0);
    CHECK(call("") > 0 && str("state", v, sizeof v) == 1 && strcmp(v, "off") != 0);
    CHECK(str("control_url", v, sizeof v) == 1 && !strcmp(v, "http://127.0.0.1:9"));

    /* logout forgets the keys */
    CHECK(call("{\"op\":\"logout\"}") > 0 && boolean("enabled") == 0);
    CHECK(access(path, F_OK) != 0);
    snprintf(path, sizeof path, "%s/settings.json", dir);
    CHECK(file_has(path, "\"enabled\":false"));

    /* start/stop cycles do not leak or wedge (lwIP is global in the core) */
    for (int i = 0; i < 3; i++) {
        CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
        CHECK(call("{\"op\":\"disable\"}") > 0 && boolean("enabled") == 0);
    }
    /* A stop that meets the node's "connection error" (its thread reports it while the stop
     * waits for that thread) must not wedge: stop at a spread of moments after a start, so some
     * land on the error from the closed port. */
    for (int i = 0; i < 200; i++) {
        CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
        usleep((useconds_t)(i % 40) * 100);
        CHECK(call("{\"op\":\"disable\"}") > 0 && boolean("enabled") == 0);
    }
    tailnet_node_shutdown();

    unlink(path);
    snprintf(path, sizeof path, "%s/tailnet-state.json", dir);
    unlink(path);
    rmdir(dir);
    printf("tailnet_node_selftest: %d passed, %d failed\n", passes, fails);
    return fails ? 1 : 0;
}
