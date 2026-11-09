/* Data-plane pieces: WireGuard handshake/transport/replay between two in-process
 * peers, disco seal/open, STUN (Go's stun_test.go vector), IPv4 helpers.
 * (Interop with a real tailscaled is testbed/m2-check.sh.) */
#include "tt.h"
#include "crypto.h"
#include "tn.h"

static void test_wg(void)
{
	uint8_t a_priv[32], a_pub[32], b_priv[32], b_pub[32];
	tn_key_new(a_priv, a_pub);
	tn_key_new(b_priv, b_pub);
	static tn_wg_self A, B;
	static tn_wg_peer a_b, b_a; /* A's view of B, B's view of A */
	tn_wg_self_init(&A, a_priv, a_pub);
	tn_wg_self_init(&B, b_priv, b_pub);
	CHECK(tn_wg_peer_init(&A, &a_b, b_pub) == 0);
	CHECK(tn_wg_peer_init(&B, &b_a, a_pub) == 0);
	CHECK(memcmp(a_b.ss, b_a.ss, 32) == 0); /* static-static DH agrees */

	uint8_t init[TN_WG_INIT_LEN], resp[TN_WG_RESP_LEN];
	int64_t t = 1000;
	tn_wg_create_init(&A, &a_b, init, 0x11111111, 1700000000LL * 1000000000LL, t);
	CHECK(init[0] == 1 && init[1] == 0 && init[2] == 0 && init[3] == 0);
	CHECK(tn_wg_check_mac1(&B, init, sizeof init) == 0);
	CHECK(tn_wg_check_mac1(&A, init, sizeof init) < 0); /* mac1 is keyed to the responder */
	uint8_t z[16] = {0};
	CHECK(memcmp(init + 132, z, 16) == 0); /* no cookie: mac2 zero */

	tn_wg_rx_init st;
	CHECK(tn_wg_init_open(&B, init, sizeof init, &st) == 0);
	CHECK(memcmp(st.peer_pub, a_pub, 32) == 0);
	CHECK(st.sender == 0x11111111);
	tn_wg_rx_init st_replay = st;
	CHECK(tn_wg_init_finish(&b_a, &st, init, t) == 0);
	/* the same initiation again: timestamp not newer */
	CHECK(tn_wg_init_finish(&b_a, &st_replay, init, t + 1000) == -2);

	tn_wg_create_resp(&b_a, &st, resp, 0x22222222, t);
	CHECK(resp[0] == 2 && tn_le32(resp + 4) == 0x22222222 && tn_le32(resp + 8) == 0x11111111);
	CHECK(b_a.next.valid && !b_a.cur.valid);
	CHECK(tn_wg_consume_resp(&A, &a_b, resp, sizeof resp, t) == 0);
	CHECK(a_b.cur.valid && a_b.cur.initiator && !a_b.hs_active);
	CHECK(memcmp(a_b.cur.send_key, b_a.next.recv_key, 32) == 0);
	CHECK(memcmp(a_b.cur.recv_key, b_a.next.send_key, 32) == 0);
	/* a second, stale response is refused */
	CHECK(tn_wg_consume_resp(&A, &a_b, resp, sizeof resp, t) < 0);

	/* transport A -> B confirms B's keypair */
	uint8_t msg[256], copy[256];
	const char *hello = "hello over wireguard";
	size_t n = tn_wg_seal(&a_b.cur, msg, (const uint8_t *)hello, strlen(hello));
	CHECK(n == 16 + 32 + 16);
	CHECK(tn_le32(msg + 4) == 0x22222222);
	memcpy(copy, msg, n);
	int m = tn_wg_open(&b_a.next, msg, n, t);
	CHECK(m == (int)32 && !memcmp(msg + 16, hello, strlen(hello)));
	CHECK(tn_wg_open(&b_a.next, copy, n, t) == -2); /* replay */
	tn_wg_confirm_next(&b_a);
	CHECK(b_a.cur.valid && !b_a.next.valid);
	/* B -> A, and a keepalive */
	n = tn_wg_seal(&b_a.cur, msg, (const uint8_t *)"pong", 4);
	CHECK(tn_wg_open(&a_b.cur, msg, n, t) == 16);
	n = tn_wg_seal(&b_a.cur, msg, NULL, 0);
	CHECK(n == 32 && tn_wg_open(&a_b.cur, msg, n, t) == 0);
	/* tamper */
	n = tn_wg_seal(&b_a.cur, msg, (const uint8_t *)"x", 1);
	msg[20] ^= 1;
	CHECK(tn_wg_open(&a_b.cur, msg, n, t) == -1);
	/* keys expire for receive after RejectAfterTime */
	n = tn_wg_seal(&b_a.cur, msg, (const uint8_t *)"late", 4);
	CHECK(tn_wg_open(&a_b.cur, msg, n, t + TN_WG_REJECT_AFTER_MS) == -1);
	CHECK(tn_wg_send_keypair(&a_b, t + TN_WG_REJECT_AFTER_MS) == NULL);
	CHECK(tn_wg_send_keypair(&a_b, t + 1) != NULL);

	/* replay window: out-of-order within the window ok, far behind refused */
	tn_wg_keypair kp;
	memset(&kp, 0, sizeof kp);
	kp.valid = 1;
	tn_random(kp.send_key, 32);
	memcpy(kp.recv_key, kp.send_key, 32);
	static uint8_t pk[3000][48];
	for (int i = 0; i < 3000; i++)
		tn_wg_seal(&kp, pk[i], (const uint8_t *)"r", 1);
	/* (the plaintext length returned is the padded one: 16) */
	CHECK(tn_wg_open(&kp, pk[2999], 48, 0) == 16);
	uint8_t dup[48];
	memcpy(dup, pk[2500], 48); /* (open decrypts in place) */
	CHECK(tn_wg_open(&kp, pk[2500], 48, 0) == 16); /* 499 behind: inside the window */
	CHECK(tn_wg_open(&kp, dup, 48, 0) == -2);      /* duplicate */
	CHECK(tn_wg_open(&kp, pk[100], 48, 0) == -2);  /* 2899 behind: outside */
	CHECK(tn_wg_open(&kp, pk[2998], 48, 0) == 16);

	/* timestamps stay increasing even if the wall clock goes back */
	uint8_t i1[TN_WG_INIT_LEN], i2[TN_WG_INIT_LEN];
	tn_wg_create_init(&A, &a_b, i1, 1, 1800000000LL * 1000000000LL, t);
	tn_wg_create_init(&A, &a_b, i2, 2, 1600000000LL * 1000000000LL, t);
	tn_wg_rx_init s1, s2;
	CHECK(tn_wg_init_open(&B, i1, sizeof i1, &s1) == 0 && tn_wg_init_finish(&b_a, &s1, i1, t + 100) == 0);
	CHECK(tn_wg_init_open(&B, i2, sizeof i2, &s2) == 0 && tn_wg_init_finish(&b_a, &s2, i2, t + 200) == 0);
	/* a flood: an initiation within 20 ms of the last is dropped */
	tn_wg_create_init(&A, &a_b, i1, 3, 1800000001LL * 1000000000LL, t);
	CHECK(tn_wg_init_open(&B, i1, sizeof i1, &s1) == 0 && tn_wg_init_finish(&b_a, &s1, i1, t + 210) == -2);
}

static void test_disco(void)
{
	uint8_t a_priv[32], a_pub[32], b_priv[32], b_pub[32], k1[32], k2[32];
	tn_key_new(a_priv, a_pub);
	tn_key_new(b_priv, b_pub);
	CHECK(tn_box_beforenm(k1, b_pub, a_priv) == 0);
	CHECK(tn_box_beforenm(k2, a_pub, b_priv) == 0);
	CHECK_MEM(k1, k2, 32);
	uint8_t pt[46] = {TN_DISCO_PING, 0}, pkt[256], out[256];
	memset(pt + 2, 0xab, 44);
	size_t n = tn_disco_seal(pkt, a_pub, k1, pt, sizeof pt);
	CHECK(n == 124); /* the smallest Ping on the wire (spec 6.1) */
	CHECK(tn_disco_is(pkt, n));
	CHECK(!memcmp(pkt, "TS\xf0\x9f\x92\xac", 6) && !memcmp(pkt + 6, a_pub, 32));
	CHECK(tn_disco_open(out, pkt, n, k2) == 46 && !memcmp(out, pt, 46));
	pkt[100] ^= 1;
	CHECK(tn_disco_open(out, pkt, n, k2) < 0);
	uint8_t e[18];
	tn_ep4 ep = {.ip = 0x01020304, .port = 41641}, back;
	tn_disco_put_ep(e, ep);
	CHECK(e[10] == 0xff && e[11] == 0xff && e[12] == 1 && e[15] == 4);
	CHECK(tn_disco_get_ep(e, &back) == 0 && back.ip == ep.ip && back.port == ep.port);
	e[0] = 0x20;
	CHECK(tn_disco_get_ep(e, &back) < 0); /* IPv6 */
}

static void test_stun(void)
{
	CHECK(tn_crc32((const uint8_t *)"123456789", 9) == 0xcbf43926u);
	uint8_t txid[12] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12}, req[TN_STUN_REQ_LEN];
	CHECK(tn_stun_request(req, txid) == 40);
	CHECK(req[0] == 0 && req[1] == 1 && tn_be16(req + 2) == 20 && tn_be32(req + 4) == 0x2112a442);
	CHECK(!memcmp(req + 24, "tailnode", 8));
	CHECK(tn_be32(req + 36) == (tn_crc32(req, 32) ^ 0x5354554eu));
	CHECK(tn_stun_is(req, sizeof req));
	/* tailscale net/stun/stun_test.go "google-1" */
	uint8_t r1[] = {0x01, 0x01, 0x00, 0x0c, 0x21, 0x12, 0xa4, 0x42, 0x23, 0x60, 0xb1, 0x1e, 0x3e, 0xc6, 0x8f, 0xfa,
			0x93, 0xe0, 0x80, 0x07, 0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xc7, 0x86, 0x69, 0x57, 0x85, 0x6f};
	tn_ep4 ep;
	uint8_t got[12];
	CHECK(tn_stun_parse(r1, sizeof r1, got, &ep) == 0);
	CHECK(ep.ip == (72u << 24 | 69u << 16 | 33u << 8 | 45u) && ep.port == 59028);
	CHECK(got[0] == 0x23 && got[11] == 0x07);
	r1[3] = 0x40; /* attribute length past the end */
	CHECK(tn_stun_parse(r1, sizeof r1, got, &ep) < 0);
}

static void test_ip(void)
{
	uint8_t pkt[128], back[128];
	tn_ep4 src = {.ip = 0x64400001, .port = 5000}, dst = {.ip = 0x64400002, .port = 9296};
	size_t n = tn_ip_build_udp(pkt, sizeof pkt, src, dst, (const uint8_t *)"datagram", 8);
	CHECK(n == 36);
	tn_ip_info ip;
	CHECK(tn_ip_parse(pkt, n, &ip) == 0);
	CHECK(ip.proto == TN_IPPROTO_UDP && ip.src == src.ip && ip.dst == dst.ip && ip.sport == 5000 &&
	      ip.dport == 9296 && ip.plen == 8 && !memcmp(ip.payload, "datagram", 8));
	memcpy(back, pkt, n);
	back[30] ^= 1; /* corrupt the payload: UDP checksum fails */
	CHECK(tn_ip_parse(back, n, &ip) < 0);
	memcpy(back, pkt, n);
	back[8] = 3; /* TTL change without a header checksum update */
	CHECK(tn_ip_parse(back, n, &ip) < 0);
	memcpy(back, pkt, n);
	back[6] = 0x20; /* more-fragments */
	back[10] = back[11] = 0;
	tn_put_be16(back + 10, tn_ip_csum(back, 20));
	CHECK(tn_ip_parse(back, n, &ip) < 0);
	n = tn_ip_build_icmp_echo(pkt, sizeof pkt, src.ip, dst.ip, 8, 0x1234, 7, (const uint8_t *)"abc", 3);
	CHECK(tn_ip_parse(pkt, n, &ip) == 0 && ip.proto == TN_IPPROTO_ICMP && ip.icmp_type == 8 && ip.sport == 0x1234 &&
	      ip.dport == 7);
	CHECK(tn_ip_parse(pkt, 19, &ip) < 0);
}

/* An echo request from a peer is answered at most TN_MTU long: an oversize one (up to what a
 * receive buffer holds) is dropped, never sealed past the data plane's buffers. */
static void test_echo_oversize(void)
{
	static uint8_t big[TN_MTU + 256], out[TN_MTU + 256], data[TN_MTU + 256];
	tn_ep4 src = {.ip = 0x64400001}, dst = {.ip = 0x64400002};
	tn_ip_info ip;
	memset(data, 0x5a, sizeof data);
	/* the largest that fits: answered, the reply exactly TN_MTU */
	size_t n = tn_ip_build_icmp_echo(big, sizeof big, src.ip, dst.ip, 8, 1, 2, data, TN_MTU - 28);
	CHECK(n == TN_MTU && tn_ip_parse(big, n, &ip) == 0 && ip.icmp_type == 8);
	size_t m = tn_dp_echo_reply(out, &ip);
	CHECK(m == TN_MTU);
	CHECK(tn_ip_parse(out, m, &ip) == 0 && ip.icmp_type == 0 && ip.src == dst.ip && ip.dst == src.ip);
	/* in place, as the data plane builds it */
	CHECK(tn_ip_parse(big, n, &ip) == 0 && tn_dp_echo_reply(big, &ip) == TN_MTU);
	/* one byte more, and the biggest a receive buffer holds: no answer */
	n = tn_ip_build_icmp_echo(big, sizeof big, src.ip, dst.ip, 8, 1, 2, data, TN_MTU - 27);
	CHECK(n == TN_MTU + 1 && tn_ip_parse(big, n, &ip) == 0 && tn_dp_echo_reply(out, &ip) == 0);
	n = tn_ip_build_icmp_echo(big, sizeof big, src.ip, dst.ip, 8, 1, 2, data, sizeof big - 28);
	CHECK(n == sizeof big && tn_ip_parse(big, n, &ip) == 0 && tn_dp_echo_reply(big, &ip) == 0);
}

/* The app's peer list (tn_set_peer_allow): everyone, nobody, or the listed tailnet IPs. */
static void test_peer_allow(void)
{
	tn_peer_allow a = {.all = 1};
	CHECK(tn_peer_allowed(&a, 0x64400001u));
	a.all = 0;
	CHECK(!tn_peer_allowed(&a, 0x64400001u)); /* an empty list lets nobody in */
	a.ip[0] = 0x64400001u;
	a.ip[1] = 0x64400009u;
	a.n = 2;
	CHECK(tn_peer_allowed(&a, 0x64400001u));
	CHECK(tn_peer_allowed(&a, 0x64400009u));
	CHECK(!tn_peer_allowed(&a, 0x64400002u));
}

/* What the peer list turned away (tn_peer_denied): one entry per address, a repeat counts and
 * moves its time, the oldest goes when all are used, newest first, and an address let in later is
 * forgotten. */
static void test_denied_log(void)
{
	tn_denied_log log;
	memset(&log, 0, sizeof log);
	tn_denied out[TN_DENIED_MAX + 2];
	CHECK(tn_denied_copy(&log, out, TN_DENIED_MAX) == 0);
	tn_denied_note(&log, 0x64400005u, 6, 9120, 100);
	tn_denied_note(&log, 0x64400006u, 17, 9302, 102);
	tn_denied_note(&log, 0x64400005u, 6, 9120, 105); /* a SYN retransmit: same slot, newest now */
	CHECK(log.n == 2);
	CHECK(tn_denied_copy(&log, out, TN_DENIED_MAX) == 2);
	CHECK(out[0].ip == 0x64400005u && out[0].count == 2 && out[0].first_s == 100 && out[0].last_s == 105);
	CHECK(out[1].ip == 0x64400006u && out[1].proto == 17 && out[1].port == 9302 && out[1].count == 1);
	/* a flood of new addresses never grows past the cap; the oldest go */
	for (uint32_t i = 0; i < 20; i++)
		tn_denied_note(&log, 0x64400100u + i, 6, 9295, 200 + i);
	CHECK(log.n == TN_DENIED_MAX);
	CHECK(tn_denied_copy(&log, out, TN_DENIED_MAX + 2) == TN_DENIED_MAX);
	CHECK(out[0].ip == 0x64400100u + 19 && out[TN_DENIED_MAX - 1].ip == 0x64400100u + 12);
	CHECK(tn_denied_copy(&log, out, 3) == 3 && out[0].ip == 0x64400100u + 19 && out[2].ip == 0x64400100u + 17);
	/* one address let in: gone from the log, the rest kept */
	tn_peer_allow a = {.all = 0, .n = 1, .ip = {0x64400100u + 19}};
	tn_denied_prune(&log, &a);
	CHECK(log.n == TN_DENIED_MAX - 1);
	CHECK(tn_denied_copy(&log, out, TN_DENIED_MAX) == TN_DENIED_MAX - 1 && out[0].ip == 0x64400100u + 18);
	/* everyone let in: empty */
	a.all = 1;
	tn_denied_prune(&log, &a);
	CHECK(log.n == 0 && tn_denied_copy(&log, out, TN_DENIED_MAX) == 0);
	/* a flood within one second: still the newest kept, in order */
	for (uint32_t i = 0; i < 20; i++)
		tn_denied_note(&log, 0x64400200u + i, 6, 9120, 500);
	CHECK(tn_denied_copy(&log, out, TN_DENIED_MAX) == TN_DENIED_MAX);
	CHECK(out[0].ip == 0x64400200u + 19 && out[TN_DENIED_MAX - 1].ip == 0x64400200u + 12);
}

/* Rest mode takes the network away: a failed poll() waits instead of spinning. */
static void test_poll_fail_wait(void)
{
	CHECK(tn_poll_fail_wait_ms(1, -1) == 0);     /* an interrupt retries at once */
	CHECK(tn_poll_fail_wait_ms(0, -1) == 100);   /* no deadline: a short pause */
	CHECK(tn_poll_fail_wait_ms(0, 5000) == 100); /* never longer than the pause */
	CHECK(tn_poll_fail_wait_ms(0, 40) == 40);    /* never past the nap's end */
	CHECK(tn_poll_fail_wait_ms(0, 0) == 1);      /* and never zero: no spin */
	CHECK(tn_poll_fail_wait_ms(0, -5) >= 1);
	int64_t t0 = tn_now_ms();
	tn_sleep_ms(30);
	CHECK(tn_now_ms() - t0 >= 25);
}

/* Rest mode breaks sockets in ways poll keeps reporting: the data plane and the control thread
 * must act on them, never poll again at once forever. */
static void test_no_spin_on_dead_sockets(void)
{
	/* the forwarder's local socket */
	CHECK(tn_fwd_local_broken(POLLERR, 0));
	CHECK(tn_fwd_local_broken(POLLNVAL, 0));
	CHECK(tn_fwd_local_broken(POLLERR | POLLIN, 1));
	CHECK(tn_fwd_local_broken(POLLHUP, 1));          /* the app is gone, data for it can never go */
	CHECK(!tn_fwd_local_broken(POLLHUP, 0));         /* an ordinary close: lwIP sends what it has */
	CHECK(!tn_fwd_local_broken(POLLHUP | POLLIN, 1)); /* the read sees EOF first */
	CHECK(!tn_fwd_local_broken(POLLHUP | POLLOUT, 1));
	CHECK(!tn_fwd_local_broken(POLLIN, 0));
	CHECK(!tn_fwd_local_broken(POLLOUT, 1));
	/* the map poll after its watchdog deadline: read (and time out), do not poll again */
	CHECK(tn_map_poll_keep_waiting(0, 0, 1000));   /* a 60 s slice ended, deadline ahead */
	CHECK(!tn_map_poll_keep_waiting(0, 0, 0));     /* deadline reached */
	CHECK(!tn_map_poll_keep_waiting(0, 0, -5000)); /* long past (the old loop spun here) */
	CHECK(!tn_map_poll_keep_waiting(1, POLLIN, 1000));
	CHECK(!tn_map_poll_keep_waiting(-1, 0, 1000)); /* poll failed: the read reports it */
}

int main(void)
{
	test_poll_fail_wait();
	test_no_spin_on_dead_sockets();
	test_peer_allow();
	test_denied_log();
	test_wg();
	test_disco();
	test_stun();
	test_ip();
	test_echo_oversize();
	return tt_done("test_dp");
}
