/* HPACK decoder tests: RFC 7541 Appendix C.4 and C.6 (hex as in
 * golang.org/x/net/http2/hpack/hpack_test.go) and blocks from Go's encoder. */
#include "tt.h"
#include "tn.h"
#include "vectors_go.h"

static char got[8192];
static size_t got_len;

static int collect(void *u, const char *n, size_t nl, const char *v, size_t vl)
{
	(void)u;
	if (got_len + nl + vl + 4 > sizeof got)
		return -1;
	memcpy(got + got_len, n, nl);
	got_len += nl;
	memcpy(got + got_len, ": ", 2);
	got_len += 2;
	memcpy(got + got_len, v, vl);
	got_len += vl;
	got[got_len++] = '\n';
	got[got_len] = 0;
	return 0;
}

static int decode(tn_hpack *h, const char *hex)
{
	static unsigned char b[1024];
	size_t n = tt_unhex(b, sizeof b, hex);
	got_len = 0;
	got[0] = 0;
	return tn_hpack_decode(h, b, n, collect, NULL);
}

static tn_hpack H;

int main(void)
{
	/* C.4 requests with Huffman, table 4096 */
	tn_hpack_init(&H, 4096);
	CHECK(decode(&H, "8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff") == 0);
	CHECK(!strcmp(got, ":method: GET\n:scheme: http\n:path: /\n:authority: www.example.com\n"));
	CHECK(H.size == 57);
	CHECK(decode(&H, "8286 84be 5886 a8eb 1064 9cbf") == 0);
	CHECK(!strcmp(got, ":method: GET\n:scheme: http\n:path: /\n:authority: www.example.com\ncache-control: no-cache\n"));
	CHECK(H.size == 110);
	CHECK(decode(&H, "8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf") == 0);
	CHECK(!strcmp(got, ":method: GET\n:scheme: https\n:path: /index.html\n:authority: www.example.com\n"
			   "custom-key: custom-value\n"));
	CHECK(H.size == 164 && H.n_ent == 3);

	/* C.6 responses with Huffman, table 256: evictions */
	tn_hpack_init(&H, 256);
	CHECK(decode(&H, "4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6"
			 "2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 e9ae 82ae 43d3") == 0);
	CHECK(!strcmp(got, ":status: 302\ncache-control: private\ndate: Mon, 21 Oct 2013 20:13:21 GMT\n"
			   "location: https://www.example.com\n"));
	CHECK(H.size == 222);
	CHECK(decode(&H, "4883 640e ffc1 c0bf") == 0);
	CHECK(!strcmp(got, ":status: 307\ncache-control: private\ndate: Mon, 21 Oct 2013 20:13:21 GMT\n"
			   "location: https://www.example.com\n"));
	CHECK(H.size == 222);
	CHECK(decode(&H, "88c1 6196 d07a be94 1054 d444 a820 0595 040b 8166 e084 a62d 1bff c05a 839b d9ab"
			 "77ad 94e7 821d d7f2 e6c7 b335 dfdf cd5b 3960 d5af 2708 7f36 72c1 ab27 0fb5 291f"
			 "9587 3160 65c0 03ed 4ee5 b106 3d50 07") == 0);
	CHECK(!strcmp(got, ":status: 200\ncache-control: private\ndate: Mon, 21 Oct 2013 20:13:22 GMT\n"
			   "location: https://www.example.com\ncontent-encoding: gzip\n"
			   "set-cookie: foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1\n"));
	CHECK(H.size == 215 && H.n_ent == 3);

	/* Go's encoder, as Go's HTTP/2 server uses it (Huffman + indexing) */
	tn_hpack_init(&H, 4096);
	for (int i = 0; i < GV_HPACK_N; i++) {
		got_len = 0;
		CHECK(tn_hpack_decode(&H, gv_hpack_blocks[i], gv_hpack_lens[i], collect, NULL) == 0);
		CHECK(!strcmp(got, gv_hpack_expect[i]));
	}

	/* Our literal encoder round-trips through the decoder */
	uint8_t enc[256];
	int n = tn_hpack_put(enc, sizeof enc, ":path", "/machine/map");
	CHECK(n > 0);
	got_len = 0;
	CHECK(tn_hpack_decode(&H, enc, (size_t)n, collect, NULL) == 0);
	CHECK(!strcmp(got, ":path: /machine/map\n"));
	CHECK(tn_hpack_put(enc, 10, ":path", "/machine/map") < 0);

	/* Malformed input is rejected, never read past the end */
	char out[64];
	static const uint8_t bad_pad[] = {0x1f, 0x00}; /* '9' (011111?) then a zero padding byte */
	CHECK(tn_huff_decode(bad_pad, sizeof bad_pad, out, sizeof out) < 0);
	static const uint8_t eos[] = {0xff, 0xff, 0xff, 0xfc}; /* 30-bit EOS inside a string */
	CHECK(tn_huff_decode(eos, sizeof eos, out, sizeof out) < 0);
	tn_hpack_init(&H, 4096);
	CHECK(decode(&H, "be") < 0);                 /* dynamic index 62 with an empty table */
	CHECK(decode(&H, "80") < 0);                 /* index 0 */
	CHECK(decode(&H, "ff ff ff ff ff 0f") < 0);  /* integer overflow */
	CHECK(decode(&H, "40 05 61 62") < 0);        /* string length past the end */
	CHECK(decode(&H, "3f e2 1f") < 0);           /* table size update to 4097: above our setting */
	CHECK(decode(&H, "3f e1 0f") == 0 && H.max_size == 2048); /* 31 + 2017 = 2048: allowed */
	/* a 5000-byte literal exceeds TN_HPACK_STR_MAX */
	static uint8_t big[5010];
	size_t o = 0;
	big[o++] = 0x00;
	big[o++] = 0x01;
	big[o++] = 'x';
	big[o++] = 0x7f;               /* 127 + continuation */
	big[o++] = (uint8_t)((5000 - 127) % 128 | 0x80);
	big[o++] = (uint8_t)((5000 - 127) / 128);
	memset(big + o, 'a', 5000 > sizeof big - o ? sizeof big - o : 5000);
	CHECK(tn_hpack_decode(&H, big, sizeof big, collect, NULL) == TN_E_TOOBIG);
	return tt_done("test_hpack");
}
