/* JSON tokenizer and MapResponse parsing: a full map captured from Headscale
 * 0.29.1 (tests/fixtures; the peer's public IP replaced with 198.51.100.39), then
 * a delta marshaled by tailcfg itself (vectors_go.h). Run from tests/. */
#include "tt.h"
#include "tn.h"
#include "vectors_go.h"

static char file[65536];
static tn_jtok toks[4096];
static tn_peer peers[8];
static tn_derp_region regions[64];
static tn_filter_rule rules[16];

static size_t load(const char *path)
{
	FILE *f = fopen(path, "rb");
	if (!f)
		return 0;
	size_t n = fread(file, 1, sizeof file - 1, f);
	fclose(f);
	file[n] = 0;
	return n;
}

static const tn_peer *peer(const tn_netmap *nm, int64_t id)
{
	for (uint32_t i = 0; i < nm->n_peers; i++)
		if (nm->peers[i].id == id)
			return &nm->peers[i];
	return NULL;
}

static uint32_t ip(uint32_t a, uint32_t b, uint32_t c, uint32_t d) { return a << 24 | b << 16 | c << 8 | d; }

static void json_tests(void)
{
	tn_json j = {.t = toks, .cap = 4096};
	const char *s = "{\"a\":[1,-2,{\"b\":null}],\"c\":\"x\\u00e9\\ud83d\\ude00\\n\",\"d\":true}";
	CHECK(tn_json_parse(&j, s, strlen(s)) == 12);
	int a = tn_json_get(&j, 0, "a");
	CHECK(tn_json_type(&j, a) == TN_J_ARR && tn_json_count(&j, a) == 3);
	int64_t v;
	int e = tn_json_first(&j, a);
	CHECK(tn_json_i64(&j, e, &v) == 0 && v == 1);
	e = tn_json_next(&j, e, a);
	CHECK(tn_json_i64(&j, e, &v) == 0 && v == -2);
	e = tn_json_next(&j, e, a);
	CHECK(tn_json_type(&j, tn_json_get(&j, e, "b")) == TN_J_NULL);
	CHECK(tn_json_next(&j, e, a) == -1);
	char buf[32];
	CHECK(tn_json_str(&j, tn_json_get(&j, 0, "c"), buf, sizeof buf) == 8);
	CHECK(!strcmp(buf, "x\xc3\xa9\xf0\x9f\x98\x80\n"));
	CHECK(tn_json_str(&j, tn_json_get(&j, 0, "c"), buf, 4) == 8 && strlen(buf) == 3); /* truncated */
	CHECK(tn_json_bool(&j, tn_json_get(&j, 0, "d")) == 1);
	CHECK(tn_json_get(&j, 0, "zz") == -1);

	static const char *bad[] = {"", "{", "{\"a\"}", "{\"a\":}", "[1,]", "[1 2]", "{\"a\":1,}", "\"\\x\"",
				    "\"abc", "{} {}", "tru", "[\"\x01\"]", "{1:2}", "]"};
	for (size_t i = 0; i < sizeof bad / sizeof bad[0]; i++) {
		int r = tn_json_parse(&j, bad[i], strlen(bad[i]));
		if (r != -1)
			fprintf(stderr, "accepted bad JSON #%zu: %s -> %d\n", i, bad[i], r);
		CHECK(r == -1);
	}
	tn_json small = {.t = toks, .cap = 3};
	CHECK(tn_json_parse(&small, "[1,2,3]", 7) == -2); /* token cap */
	char deep[200];
	memset(deep, '[', 100);
	memset(deep + 100, ']', 100);
	CHECK(tn_json_parse(&j, deep, 200) == -2); /* depth cap */

	char w[64];
	tn_jw jw;
	tn_jw_init(&jw, w, sizeof w);
	tn_jw_obj(&jw);
	tn_jw_kstr(&jw, "k", "a\"b\\\n");
	tn_jw_kint(&jw, "n", -5);
	tn_jw_end(&jw);
	CHECK(!jw.err && !strcmp(w, "{\"k\":\"a\\\"b\\\\\\u000a\",\"n\":-5}"));
	tn_jw_init(&jw, w, 8);
	tn_jw_kstr(&jw, "long", "value");
	CHECK(jw.err);
}

static void util_tests(void)
{
	tn_prefix4 p;
	tn_ep4 ep;
	CHECK(tn_parse_prefix4("100.64.0.0/10", 13, &p) == 0 && p.addr == ip(100, 64, 0, 0) && p.bits == 10);
	CHECK(tn_parse_prefix4("1.2.3.4/33", 10, &p) < 0);
	CHECK(tn_parse_prefix4("fd7a::1/128", 11, &p) < 0);
	CHECK(tn_parse_ep4("192.168.1.2:41641", 17, &ep) == 0 && ep.ip == ip(192, 168, 1, 2) && ep.port == 41641);
	CHECK(tn_parse_ep4("[2001:db8::1]:41641", 19, &ep) < 0);
	CHECK(tn_parse_ep4("1.2.3.4:0", 9, &ep) < 0);
	CHECK(tn_parse_ep4("1.2.3.256:1", 11, &ep) < 0);
	CHECK(tn_parse_time("2027-01-02T03:04:05Z", 20) == 1798859045);
	CHECK(tn_parse_time("2026-11-07T22:00:00.123456789+01:00", 35) == 1794088800 - 3600);
	CHECK(tn_parse_time("0001-01-01T00:00:00Z", 20) == 0);
	CHECK(tn_parse_time("garbage", 7) < 0);
}

static void map_tests(void)
{
	static tn_nmstate s;
	tn_json j = {.t = toks, .cap = 4096};
	tn_map_result res;
	s.nm.peers = peers;
	s.nm.regions = regions;
	s.nm.rules = rules;
	s.max_peers = 8;
	s.max_regions = 64;
	s.max_rules = 16;
	tn_netmap_reset(&s);

	size_t n = load("fixtures/headscale-0.29.1-map-full.json");
	CHECK(n > 1000);
	CHECK(tn_json_parse(&j, file, n) > 0);
	CHECK(tn_netmap_apply(&s, &j, &res) == 0 && res.changed);
	const tn_netmap *nm = &s.nm;
	CHECK(nm->self_id == 2 && nm->self_addr4 == ip(100, 64, 0, 2));
	CHECK(!strcmp(nm->self_name, "tn-mac.tnbed.internal."));
	CHECK(nm->n_peers == 1);
	const tn_peer *p = peer(nm, 1);
	CHECK(p && p->addr4 == ip(100, 64, 0, 1) && !strcmp(p->name, "linux-peer.tnbed.internal."));
	CHECK(p && p->home_derp == 2 && p->online == 1 && p->has_disco && p->n_ep == 2);
	CHECK(p && p->ep[0].ip == ip(198, 51, 100, 39) && p->ep[0].port == 50890);
	CHECK(p && p->n_allowed == 1 && p->allowed[0].addr == ip(100, 64, 0, 1) && p->allowed[0].bits == 32);
	CHECK(p && p->key[0] == 0x22 && p->key[31] == 0x65 && p->disco[0] == 0xaa);
	CHECK(nm->n_regions >= 20);
	CHECK(nm->regions[0].id == 1 && nm->regions[0].n_nodes >= 1 && nm->regions[0].nodes[0].ipv4 != 0);
	CHECK(nm->regions[0].nodes[0].stun_port == 3478 && nm->regions[0].nodes[0].derp_port == 443);
	CHECK(nm->filter_known && nm->n_rules == 1);
	CHECK(nm->rules[0].src[0].lo == 0 && nm->rules[0].src[0].hi == 0xffffffffu);
	CHECK(nm->rules[0].dst[0].port_lo == 0 && nm->rules[0].dst[0].port_hi == 65535);
	CHECK(nm->control_time > 1794000000);

	/* The tailcfg-marshaled delta */
	CHECK(tn_json_parse(&j, gv_map_delta, strlen(gv_map_delta)) > 0);
	CHECK(tn_netmap_apply(&s, &j, &res) == 0 && res.changed);
	CHECK(nm->n_peers == 2);
	p = peer(nm, 7);
	CHECK(p && !strcmp(p->name, "phone.tnbed.internal.") && p->addr4 == ip(100, 64, 0, 7));
	CHECK(p && p->home_derp == 2 && p->online == 1 && p->key[0] == 0xab && p->disco[31] == 0xcd);
	CHECK(p && p->n_ep == 1 && p->ep[0].ip == ip(192, 168, 1, 20)); /* IPv6 endpoint skipped */
	CHECK(p && p->key_expiry == 1798859045);
	p = peer(nm, 1);
	CHECK(p && p->home_derp == 9 && p->n_ep == 1 && p->ep[0].ip == ip(203, 0, 113, 9) && p->ep[0].port == 12345);
	CHECK(nm->n_rules == 1 && nm->rules[0].n_src == 2 && nm->rules[0].n_dst == 1);
	CHECK(nm->rules[0].src[0].lo == ip(100, 64, 0, 0) && nm->rules[0].src[0].hi == ip(100, 127, 255, 255));
	CHECK(nm->rules[0].src[1].lo == ip(100, 64, 0, 7) && nm->rules[0].src[1].hi == ip(100, 64, 0, 9));
	CHECK(nm->rules[0].dst[0].port_lo == 9295 && nm->rules[0].dst[0].port_hi == 9296);
	CHECK(nm->rules[0].n_proto == 2 && nm->rules[0].proto[0] == 6 && nm->rules[0].proto[1] == 17);
	CHECK(nm->regions[0].id == 1); /* DERPMap absent in the delta: unchanged */

	/* Removal, keep-alive, the "*" filter reset and an expired self node */
	const char *rm = "{\"PeersRemoved\":[7],\"PacketFilters\":{\"*\":null}}";
	CHECK(tn_json_parse(&j, rm, strlen(rm)) > 0 && tn_netmap_apply(&s, &j, &res) == 0);
	CHECK(nm->n_peers == 1 && peer(nm, 7) == NULL && nm->n_rules == 0);
	const char *ka = "{\"KeepAlive\":true}";
	CHECK(tn_json_parse(&j, ka, strlen(ka)) > 0 && tn_netmap_apply(&s, &j, &res) == 0);
	CHECK(res.keepalive_only && !res.changed);
	const char *ex = "{\"Node\":{\"ID\":2,\"Expired\":true,\"Addresses\":[\"100.64.0.2/32\"]}}";
	CHECK(tn_json_parse(&j, ex, strlen(ex)) > 0 && tn_netmap_apply(&s, &j, &res) == 0 && res.self_expired);

	/* Caps: more peers than max_peers is an error, not an overflow */
	char big[4096];
	size_t o = (size_t)snprintf(big, sizeof big, "{\"Peers\":[");
	for (int i = 1; i <= 9; i++)
		o += (size_t)snprintf(big + o, sizeof big - o, "%s{\"ID\":%d}", i > 1 ? "," : "", i);
	snprintf(big + o, sizeof big - o, "]}");
	CHECK(tn_json_parse(&j, big, strlen(big)) > 0 && tn_netmap_apply(&s, &j, &res) == TN_E_TOOBIG);
}

int main(void)
{
	json_tests();
	util_tests();
	map_tests();
	return tt_done("test_netmap");
}
