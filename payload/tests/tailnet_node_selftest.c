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
    /* rename: any time; kept while off, and the node starts again under it while on */
    CHECK(call("{\"op\":\"rename\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":\"rename\",\"hostname\":\"den-ps5u\"}") > 0 && boolean("enabled") == 0);
    CHECK(str("hostname", v, sizeof v) == 1 && !strcmp(v, "den-ps5u"));
    CHECK(file_has(path, "den-ps5u"));
    CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
    CHECK(call("{\"op\":\"rename\",\"hostname\":\"living-room-ps5u\"}") > 0 && boolean("enabled") == 1);
    CHECK(str("hostname", v, sizeof v) == 1 && !strcmp(v, "living-room-ps5u"));
    CHECK(str("state", v, sizeof v) == 1 && strcmp(v, "off") != 0);
    CHECK(boolean("signed_in") == 1);

    /* renew: the same node signs in again; refused while off */
    CHECK(call("{\"op\":\"renew\"}") > 0 && boolean("ok") == 1 && boolean("enabled") == 1);
    CHECK(call("{\"op\":\"disable\"}") > 0 && boolean("enabled") == 0);
    CHECK(call("{\"op\":\"renew\"}") > 0 && strstr(out, "not_running"));

    /* the allow-list: a fresh setup lets in only listed devices; "all" is an explicit choice */
    CHECK(call("") > 0 && str("allow_mode", v, sizeof v) == 1 && !strcmp(v, "listed"));
    CHECK(!tailnet_node_allows(0x64400005u));
    CHECK(call("{\"op\":\"allow_add\",\"ip\":\"100.64.0.5\"}") > 0 && boolean("ok") == 1);
    CHECK(call("{\"op\":\"allow_add\",\"ip\":\"100.64.0.5\"}") > 0); /* twice: listed once */
    CHECK(str("allow", v, sizeof v) == 1 && !strcmp(v, "100.64.0.5"));
    CHECK(tailnet_node_allows(0x64400005u) && !tailnet_node_allows(0x64400006u));
    CHECK(call("{\"op\":\"allow_add\",\"ip\":\"192.168.1.5\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":\"allow_add\",\"ip\":\"100.128.0.1\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":\"allow\",\"allow_mode\":\"listed\",\"allow\":\"100.64.0.5,100.101.2.3, 100.64.0.5\"}") > 0);
    CHECK(str("allow", v, sizeof v) == 1 && !strcmp(v, "100.64.0.5,100.101.2.3"));
    CHECK(call("{\"op\":\"allow\",\"allow\":\"100.64.0.5,nope\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":\"allow\",\"allow_mode\":\"some\"}") > 0 && strstr(out, "bad_request"));
    CHECK(call("{\"op\":\"allow\",\"allow_mode\":\"all\",\"allow\":\"100.64.0.5\"}") > 0);
    CHECK(str("allow_mode", v, sizeof v) == 1 && !strcmp(v, "all") && tailnet_node_allows(0x64400077u));
    CHECK(call("{\"op\":\"allow\",\"allow_mode\":\"listed\",\"allow\":\"100.64.0.5,100.101.2.3\"}") > 0);
    /* kept across a restart, and pushed into a running node */
    tailnet_node_shutdown();
    tailnet_node_set_dir(dir);
    CHECK(call("") > 0 && str("allow_mode", v, sizeof v) == 1 && !strcmp(v, "listed"));
    CHECK(str("allow", v, sizeof v) == 1 && !strcmp(v, "100.64.0.5,100.101.2.3"));
    CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
    CHECK(tailnet_node_allows(0x64650203u) && !tailnet_node_allows(0x64400006u));
    CHECK(call("{\"op\":\"disable\"}") > 0);

    /* settings saved before the allow-list (12.0.0): every device still passes until the node
     * runs with its netmap and the list is started from the devices on the tailnet */
    {
        FILE *f = fopen(path, "w");
        CHECK(f != NULL);
        if (f) {
            fputs("{\"enabled\":false,\"control_url\":\"http://127.0.0.1:9\",\"hostname\":\"old\",\"control_addr\":\"\"}\n", f);
            fclose(f);
        }
        tailnet_node_set_dir(dir);
        CHECK(call("") > 0 && str("allow_mode", v, sizeof v) == 1 && !strcmp(v, "all"));
        CHECK(tailnet_node_allows(0x64400009u));
    }

    /* Rest mode, in every state the host can reach. The PS5 freezes the process and the core
     * notices the wall clock jump on wake: tailnet_node_test_rest_wake runs that same wake
     * sequence. On: it rejoins (state not off); paused: nothing runs and it stays paused;
     * signed out: it stays signed out. Interleaved with pause/resume, renames and list changes;
     * ASan/UBSan and the 200-cycle loop catch a leak, a wedge or a use-after-free. */
    for (int i = 0; i < 60; i++) {
        CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
        usleep((useconds_t)(i % 10) * 300);
        tailnet_node_test_rest_wake(); /* on, mid connect */
        CHECK(call("") > 0 && str("state", v, sizeof v) == 1 && strcmp(v, "off") != 0);
        if (i % 3 == 0) {
            CHECK(call("{\"op\":\"renew\"}") > 0 && boolean("ok") == 1);
            tailnet_node_test_rest_wake(); /* mid sign-in renewal */
        }
        if (i % 4 == 0) {
            char req[128];
            snprintf(req, sizeof req, "{\"op\":\"rename\",\"hostname\":\"ps5-%d\"}", i);
            CHECK(call(req) > 0 && boolean("enabled") == 1);
            tailnet_node_test_rest_wake();
        }
        if (i % 5 == 0) CHECK(call("{\"op\":\"allow_add\",\"ip\":\"100.64.1.1\"}") > 0);
        CHECK(call("{\"op\":\"disable\"}") > 0 && boolean("enabled") == 0);
        tailnet_node_test_rest_wake(); /* paused: nothing to wake */
        CHECK(call("") > 0 && str("state", v, sizeof v) == 1 && !strcmp(v, "off") && boolean("enabled") == 0);
    }
    /* a restart while paused stays paused; switched on later it starts cleanly */
    tailnet_node_shutdown();
    tailnet_node_set_dir(dir);
    CHECK(tailnet_node_boot() == 0);
    CHECK(call("") > 0 && boolean("enabled") == 0 && str("state", v, sizeof v) == 1 && !strcmp(v, "off"));
    CHECK(call("{\"op\":\"enable\"}") > 0 && boolean("enabled") == 1);
    for (int i = 0; i < 100; i++) tailnet_node_test_rest_wake(); /* many wakes in a row */
    CHECK(call("") > 0 && str("state", v, sizeof v) == 1 && strcmp(v, "off") != 0);
    /* signed out: rest and wake leave it signed out and off */
    CHECK(call("{\"op\":\"logout\"}") > 0 && boolean("enabled") == 0 && boolean("signed_in") == 0);
    tailnet_node_test_rest_wake();
    CHECK(call("") > 0 && boolean("enabled") == 0 && boolean("signed_in") == 0);
    tailnet_node_shutdown();

    unlink(path);
    snprintf(path, sizeof path, "%s/tailnet-state.json", dir);
    unlink(path);
    rmdir(dir);
    printf("tailnet_node_selftest: %d passed, %d failed\n", passes, fails);
    return fails ? 1 : 0;
}
