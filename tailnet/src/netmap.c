/* MapResponse -> tn_netmap (spec 3.9). Field names and delta semantics follow
 * tailscale tailcfg/tailcfg.go and control/controlclient/map.go (BSD-3-Clause). */
#include "tn.h"

#include <string.h>

#define S(jj, i) ((jj)->s + (jj)->t[i].start)
#define L(jj, i) ((size_t)((jj)->t[i].end - (jj)->t[i].start))

void tn_netmap_reset(tn_nmstate *s)
{
	tn_peer *p = s->nm.peers;
	tn_derp_region *r = s->nm.regions;
	tn_filter_rule *f = s->nm.rules;
	memset(&s->nm, 0, sizeof s->nm);
	s->nm.peers = p;
	s->nm.regions = r;
	s->nm.rules = f;
	s->n_sets = 0;
}

static int key_field(const tn_json *j, int obj, const char *name, const char *prefix, uint8_t k[32])
{
	int t = tn_json_get(j, obj, name);
	if (tn_json_type(j, t) != TN_J_STR)
		return -1;
	return tn_key_parse(k, prefix, S(j, t), L(j, t));
}

static void time_field(const tn_json *j, int t, int64_t *out)
{
	if (tn_json_type(j, t) == TN_J_STR) {
		int64_t v = tn_parse_time(S(j, t), L(j, t));
		if (v >= 0)
			*out = v;
	}
}

static void prefixes(const tn_json *j, int arr, tn_prefix4 *out, uint8_t *n, uint8_t max)
{
	*n = 0;
	for (int e = tn_json_first(j, arr); e >= 0 && *n < max; e = tn_json_next(j, e, arr))
		if (tn_json_type(j, e) == TN_J_STR && tn_parse_prefix4(S(j, e), L(j, e), &out[*n]) == 0)
			(*n)++;
}

static void endpoints(const tn_json *j, int arr, tn_peer *p)
{
	p->n_ep = 0;
	for (int e = tn_json_first(j, arr); e >= 0 && p->n_ep < TN_PEER_EP_MAX; e = tn_json_next(j, e, arr))
		if (tn_json_type(j, e) == TN_J_STR && tn_parse_ep4(S(j, e), L(j, e), &p->ep[p->n_ep]) == 0)
			p->n_ep++;
}

/* tailcfg.Node -> tn_peer. Returns -1 without an ID. */
static int parse_node(const tn_json *j, int o, tn_peer *p)
{
	int t;
	memset(p, 0, sizeof *p);
	p->online = -1;
	if (tn_json_i64(j, tn_json_get(j, o, "ID"), &p->id) < 0)
		return -1;
	tn_json_str(j, tn_json_get(j, o, "Name"), p->name, sizeof p->name);
	key_field(j, o, "Key", "nodekey:", p->key);
	p->has_disco = key_field(j, o, "DiscoKey", "discokey:", p->disco) == 0;
	time_field(j, tn_json_get(j, o, "KeyExpiry"), &p->key_expiry);

	tn_prefix4 addrs[TN_PEER_ALLOWED_MAX];
	uint8_t na = 0;
	prefixes(j, tn_json_get(j, o, "Addresses"), addrs, &na, TN_PEER_ALLOWED_MAX);
	for (uint8_t i = 0; i < na; i++)
		if (addrs[i].bits == 32) {
			p->addr4 = addrs[i].addr;
			break;
		}
	t = tn_json_get(j, o, "AllowedIPs");
	if (tn_json_type(j, t) == TN_J_ARR) {
		prefixes(j, t, p->allowed, &p->n_allowed, TN_PEER_ALLOWED_MAX);
	} else { /* nil AllowedIPs = Addresses (capver 112) */
		memcpy(p->allowed, addrs, sizeof addrs);
		p->n_allowed = na;
	}
	endpoints(j, tn_json_get(j, o, "Endpoints"), p);

	int64_t v;
	if (tn_json_i64(j, tn_json_get(j, o, "HomeDERP"), &v) == 0 && v > 0 && v < 0x7fffffff)
		p->home_derp = (int32_t)v;
	t = tn_json_get(j, o, "DERP"); /* legacy "127.3.3.40:<region>" */
	if (!p->home_derp && tn_json_type(j, t) == TN_J_STR && L(j, t) > 11 &&
	    !memcmp(S(j, t), "127.3.3.40:", 11)) {
		int32_t r = 0;
		for (size_t i = 11; i < L(j, t) && S(j, t)[i] >= '0' && S(j, t)[i] <= '9' && r < 100000000; i++)
			r = r * 10 + (S(j, t)[i] - '0');
		p->home_derp = r;
	}
	int b = tn_json_bool(j, tn_json_get(j, o, "Online"));
	if (b >= 0)
		p->online = (int8_t)b;
	p->expired = tn_json_bool(j, tn_json_get(j, o, "Expired")) == 1;
	p->wg_only = tn_json_bool(j, tn_json_get(j, o, "IsWireGuardOnly")) == 1;
	return 0;
}

static tn_peer *find_peer(tn_nmstate *s, int64_t id)
{
	for (uint32_t i = 0; i < s->nm.n_peers; i++)
		if (s->nm.peers[i].id == id)
			return &s->nm.peers[i];
	return NULL;
}

static int upsert(tn_nmstate *s, const tn_peer *p)
{
	tn_peer *d = find_peer(s, p->id);
	if (!d) {
		if (s->nm.n_peers >= s->max_peers)
			return TN_E_TOOBIG;
		d = &s->nm.peers[s->nm.n_peers++];
	}
	*d = *p;
	return TN_OK;
}

static void sort_peers(tn_nmstate *s)
{
	for (uint32_t i = 1; i < s->nm.n_peers; i++) {
		tn_peer t = s->nm.peers[i];
		uint32_t k = i;
		while (k > 0 && s->nm.peers[k - 1].id > t.id) {
			s->nm.peers[k] = s->nm.peers[k - 1];
			k--;
		}
		s->nm.peers[k] = t;
	}
}

static void apply_patch(tn_nmstate *s, const tn_json *j, int o)
{
	int64_t id;
	if (tn_json_i64(j, tn_json_get(j, o, "NodeID"), &id) < 0)
		return;
	tn_peer *p = find_peer(s, id);
	if (!p)
		return; /* unknown node: ignore (tailcfg.PeerChange doc) */
	int64_t v;
	int t;
	if (tn_json_i64(j, tn_json_get(j, o, "DERPRegion"), &v) == 0 && v > 0 && v < 0x7fffffff)
		p->home_derp = (int32_t)v;
	if (tn_json_type(j, t = tn_json_get(j, o, "Endpoints")) == TN_J_ARR)
		endpoints(j, t, p);
	key_field(j, o, "Key", "nodekey:", p->key);
	if (key_field(j, o, "DiscoKey", "discokey:", p->disco) == 0)
		p->has_disco = 1;
	int b = tn_json_bool(j, tn_json_get(j, o, "Online"));
	if (b >= 0)
		p->online = (int8_t)b;
	time_field(j, tn_json_get(j, o, "KeyExpiry"), &p->key_expiry);
}

static int parse_derpmap(tn_nmstate *s, const tn_json *j, int dm)
{
	int regs = tn_json_get(j, dm, "Regions");
	tn_netmap *nm = &s->nm;
	nm->n_regions = 0;
	for (int k = tn_json_first(j, regs); k >= 0; k = tn_json_next(j, k, regs)) {
		int r = k + 1;
		int64_t v;
		if (tn_json_type(j, r) != TN_J_OBJ || nm->n_regions >= s->max_regions)
			continue;
		tn_derp_region *g = &nm->regions[nm->n_regions];
		memset(g, 0, sizeof *g);
		if (tn_json_i64(j, tn_json_get(j, r, "RegionID"), &v) < 0 || v <= 0 || v > 0x7fffffff)
			continue;
		g->id = (int32_t)v;
		tn_json_str(j, tn_json_get(j, r, "RegionCode"), g->code, sizeof g->code);
		tn_json_str(j, tn_json_get(j, r, "RegionName"), g->name, sizeof g->name);
		g->avoid = tn_json_bool(j, tn_json_get(j, r, "Avoid")) == 1;
		g->no_measure_no_home = tn_json_bool(j, tn_json_get(j, r, "NoMeasureNoHome")) == 1;
		int nodes = tn_json_get(j, r, "Nodes");
		for (int e = tn_json_first(j, nodes); e >= 0 && g->n_nodes < TN_DERP_NODES_MAX;
		     e = tn_json_next(j, e, nodes)) {
			tn_derp_node *d = &g->nodes[g->n_nodes];
			char ip[64];
			memset(d, 0, sizeof *d);
			tn_json_str(j, tn_json_get(j, e, "Name"), d->name, sizeof d->name);
			tn_json_str(j, tn_json_get(j, e, "HostName"), d->host, sizeof d->host);
			tn_json_str(j, tn_json_get(j, e, "CertName"), d->cert_name, sizeof d->cert_name);
			int il = tn_json_str(j, tn_json_get(j, e, "IPv4"), ip, sizeof ip);
			if (il > 0 && (size_t)il < sizeof ip)
				tn_parse_ip4(ip, (size_t)il, &d->ipv4);
			d->stun_port = 3478; /* 0 = default, -1 = none (tailcfg/derpmap.go) */
			if (tn_json_i64(j, tn_json_get(j, e, "STUNPort"), &v) == 0 && v != 0 && v >= -1 && v < 65536)
				d->stun_port = (int32_t)v;
			d->derp_port = 443;
			if (tn_json_i64(j, tn_json_get(j, e, "DERPPort"), &v) == 0 && v > 0 && v < 65536)
				d->derp_port = (int32_t)v;
			d->stun_only = tn_json_bool(j, tn_json_get(j, e, "STUNOnly")) == 1;
			d->insecure_for_tests = tn_json_bool(j, tn_json_get(j, e, "InsecureForTests")) == 1;
			d->can_port80 = tn_json_bool(j, tn_json_get(j, e, "CanPort80")) == 1;
			if (d->host[0])
				g->n_nodes++;
		}
		nm->n_regions++;
	}
	/* Regions arrive keyed by id in a JSON object; keep them sorted by id. */
	for (uint32_t i = 1; i < nm->n_regions; i++) {
		tn_derp_region t = nm->regions[i];
		uint32_t k = i;
		while (k > 0 && nm->regions[k - 1].id > t.id) {
			nm->regions[k] = nm->regions[k - 1];
			k--;
		}
		nm->regions[k] = t;
	}
	return TN_OK;
}

/* "*", "a.b.c.d", "a.b.c.d/n", "a.b.c.d-e.f.g.h"; IPv6 and cap: forms are skipped. */
static int parse_range(const char *s, size_t n, tn_range4 *r)
{
	if (n == 1 && s[0] == '*') {
		r->lo = 0;
		r->hi = 0xffffffffu;
		return 0;
	}
	const char *dash = memchr(s, '-', n);
	if (dash) {
		size_t a = (size_t)(dash - s);
		if (tn_parse_ip4(s, a, &r->lo) < 0 || tn_parse_ip4(dash + 1, n - a - 1, &r->hi) < 0 || r->hi < r->lo)
			return -1;
		return 0;
	}
	tn_prefix4 p;
	if (tn_parse_prefix4(s, n, &p) < 0)
		return -1;
	uint32_t mask = p.bits ? 0xffffffffu << (32 - p.bits) : 0;
	r->lo = p.addr & mask;
	r->hi = r->lo | ~mask;
	return 0;
}

static int set_index(tn_nmstate *s, const char *name, size_t n)
{
	for (uint32_t i = 0; i < s->n_sets; i++)
		if (strlen(s->sets[i]) == n && !memcmp(s->sets[i], name, n))
			return (int)i;
	if (s->n_sets >= sizeof s->sets / sizeof s->sets[0] || n >= sizeof s->sets[0])
		return -1;
	memcpy(s->sets[s->n_sets], name, n);
	s->sets[s->n_sets][n] = 0;
	return (int)s->n_sets++;
}

static void drop_set(tn_nmstate *s, int set)
{
	uint32_t o = 0;
	for (uint32_t i = 0; i < s->nm.n_rules; i++)
		if (s->nm.rules[i].set != set)
			s->nm.rules[o++] = s->nm.rules[i];
	s->nm.n_rules = o;
}

static int parse_rules(tn_nmstate *s, const tn_json *j, int arr, int set)
{
	drop_set(s, set);
	for (int e = tn_json_first(j, arr); e >= 0; e = tn_json_next(j, e, arr)) {
		if (s->nm.n_rules >= s->max_rules)
			return TN_E_TOOBIG;
		tn_filter_rule *r = &s->nm.rules[s->nm.n_rules];
		memset(r, 0, sizeof *r);
		r->set = (uint8_t)set;
		int a = tn_json_get(j, e, "SrcIPs");
		for (int x = tn_json_first(j, a); x >= 0 && r->n_src < TN_FILTER_SRC_MAX; x = tn_json_next(j, x, a))
			if (tn_json_type(j, x) == TN_J_STR && parse_range(S(j, x), L(j, x), &r->src[r->n_src]) == 0)
				r->n_src++;
		a = tn_json_get(j, e, "DstPorts");
		for (int x = tn_json_first(j, a); x >= 0 && r->n_dst < TN_FILTER_DST_MAX; x = tn_json_next(j, x, a)) {
			int ip = tn_json_get(j, x, "IP"), ports = tn_json_get(j, x, "Ports");
			int64_t f = 0, l = 0;
			if (tn_json_type(j, ip) != TN_J_STR || parse_range(S(j, ip), L(j, ip), &r->dst[r->n_dst].ip) < 0)
				continue;
			if (tn_json_i64(j, tn_json_get(j, ports, "First"), &f) < 0 ||
			    tn_json_i64(j, tn_json_get(j, ports, "Last"), &l) < 0 || f < 0 || l > 65535 || f > l)
				continue;
			r->dst[r->n_dst].port_lo = (uint16_t)f;
			r->dst[r->n_dst].port_hi = (uint16_t)l;
			r->n_dst++;
		}
		a = tn_json_get(j, e, "IPProto");
		for (int x = tn_json_first(j, a); x >= 0 && r->n_proto < TN_FILTER_PROTO_MAX; x = tn_json_next(j, x, a)) {
			int64_t v;
			if (tn_json_i64(j, x, &v) == 0 && v >= 0 && v < 256)
				r->proto[r->n_proto++] = (uint8_t)v;
		}
		/* A rule with no IPv4 source or destination can never match us: skip it. */
		if (r->n_src && r->n_dst)
			s->nm.n_rules++;
	}
	return TN_OK;
}

int tn_netmap_apply(tn_nmstate *s, const tn_json *j, tn_map_result *res)
{
	tn_netmap *nm = &s->nm;
	int t, r, any = 0;
	tn_peer p;
	memset(res, 0, sizeof *res);
	if (tn_json_type(j, 0) != TN_J_OBJ)
		return TN_E_PROTO;

	if (tn_json_type(j, t = tn_json_get(j, 0, "Node")) == TN_J_OBJ && parse_node(j, t, &p) == 0) {
		nm->self_id = p.id;
		memcpy(nm->self_name, p.name, sizeof nm->self_name);
		nm->self_addr4 = p.addr4;
		nm->self_home_derp = p.home_derp;
		nm->self_key_expiry = p.key_expiry;
		nm->self_expired = p.expired;
		res->self_expired = p.expired;
		any = 1;
	}
	if (tn_json_type(j, t = tn_json_get(j, 0, "DERPMap")) == TN_J_OBJ) {
		parse_derpmap(s, j, t);
		any = 1;
	}
	if (tn_json_type(j, t = tn_json_get(j, 0, "Peers")) == TN_J_ARR) {
		/* A full list replaces everything; PeersChanged/Removed are then ignored. */
		nm->n_peers = 0;
		for (int e = tn_json_first(j, t); e >= 0; e = tn_json_next(j, e, t))
			if (parse_node(j, e, &p) == 0 && (r = upsert(s, &p)) < 0)
				return r;
		any = 1;
	} else {
		if (tn_json_type(j, t = tn_json_get(j, 0, "PeersChanged")) == TN_J_ARR) {
			for (int e = tn_json_first(j, t); e >= 0; e = tn_json_next(j, e, t))
				if (parse_node(j, e, &p) == 0 && (r = upsert(s, &p)) < 0)
					return r;
			any = 1;
		}
		if (tn_json_type(j, t = tn_json_get(j, 0, "PeersRemoved")) == TN_J_ARR) {
			for (int e = tn_json_first(j, t); e >= 0; e = tn_json_next(j, e, t)) {
				int64_t id;
				tn_peer *d;
				if (tn_json_i64(j, e, &id) == 0 && (d = find_peer(s, id)) != NULL) {
					*d = nm->peers[--nm->n_peers];
				}
			}
			any = 1;
		}
	}
	if (tn_json_type(j, t = tn_json_get(j, 0, "PeersChangedPatch")) == TN_J_ARR) {
		for (int e = tn_json_first(j, t); e >= 0; e = tn_json_next(j, e, t))
			apply_patch(s, j, e);
		any = 1;
	}
	if (any)
		sort_peers(s);

	if (tn_json_type(j, t = tn_json_get(j, 0, "PacketFilter")) == TN_J_ARR) {
		int set = set_index(s, "base", 4);
		if (set >= 0 && (r = parse_rules(s, j, t, set)) < 0)
			return r;
		nm->filter_known = 1;
		any = 1;
	}
	if (tn_json_type(j, t = tn_json_get(j, 0, "PacketFilters")) == TN_J_OBJ) {
		int star = tn_json_get(j, t, "*");
		if (star >= 0 && tn_json_type(j, star) == TN_J_NULL) {
			nm->n_rules = 0;
			s->n_sets = 0;
		}
		for (int k = tn_json_first(j, t); k >= 0; k = tn_json_next(j, k, t)) {
			if (L(j, k) == 1 && S(j, k)[0] == '*')
				continue;
			int set = set_index(s, S(j, k), L(j, k));
			if (set < 0)
				continue;
			if (tn_json_type(j, k + 1) == TN_J_ARR) {
				if ((r = parse_rules(s, j, k + 1, set)) < 0)
					return r;
			} else {
				drop_set(s, set);
			}
		}
		nm->filter_known = 1;
		any = 1;
	}
	time_field(j, tn_json_get(j, 0, "ControlTime"), &nm->control_time);
	tn_json_str(j, tn_json_get(j, 0, "PopBrowserURL"), res->pop_url, sizeof res->pop_url);

	res->keepalive_only = !any && tn_json_bool(j, tn_json_get(j, 0, "KeepAlive")) == 1;
	res->changed = (uint8_t)any;
	if (any)
		nm->updates++;
	return TN_OK;
}
