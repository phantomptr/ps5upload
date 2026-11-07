/* Crypto known-answer tests. Sources are cited per vector. */
#include "tt.h"
#include "crypto.h"
#include "tn.h"
#include "vectors_go.h"

static unsigned char A[4096], B[4096], C[4096];

static void blake2s_tests(void)
{
	uint8_t out[32];
	/* RFC 7693 Appendix B: BLAKE2s-256("abc") */
	tn_b2s256(out, "abc", 3);
	tt_unhex(A, sizeof A, "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982");
	CHECK_MEM(out, A, 32);

	/* BLAKE2 reference KAT (blake2s-kat.txt, keyed: key = 00..1f, in = 00..len-1);
	 * the values below were cross-checked with Python's hashlib.blake2s. */
	static const struct { size_t n; const char *hex; } kat[] = {
		{0, "48a8997da407876b3d79c0d92325ad3b89cbb754d86ab71aee047ad345fd2c49"},
		{1, "40d15fee7c328830166ac3f918650f807e7e01e177258cdc0a39b11f598066f1"},
		{2, "6bb71300644cd3991b26ccd4d274acd1adeab8b1d7914546c1198bbe9fc9d803"},
		{63, "c65382513f07460da39833cb666c5ed82e61b9e998f4b0c4287cee56c3cc9bcd"},
		{64, "8975b0577fd35566d750b362b0897a26c399136df07bababbde6203ff2954ed4"},
		{65, "21fe0ceb0052be7fb0f004187cacd7de67fa6eb0938d927677f2398c132317a8"},
		{255, "3fb735061abc519dfe979e54c1ee5bfad0a9d858b3315bad34bde999efd724dd"},
	};
	uint8_t key[32], in[256];
	for (int i = 0; i < 32; i++)
		key[i] = (uint8_t)i;
	for (int i = 0; i < 256; i++)
		in[i] = (uint8_t)i;
	for (size_t k = 0; k < sizeof kat / sizeof kat[0]; k++) {
		tn_b2s s;
		tn_b2s_init(&s, 32, key, 32);
		/* feed in two pieces to exercise buffering */
		tn_b2s_update(&s, in, kat[k].n / 3);
		tn_b2s_update(&s, in + kat[k].n / 3, kat[k].n - kat[k].n / 3);
		tn_b2s_final(&s, out);
		tt_unhex(A, sizeof A, kat[k].hex);
		CHECK_MEM(out, A, 32);
	}

	/* Keyed BLAKE2s-128 (WireGuard MAC1 shape) and HMAC-BLAKE2s: golang.org/x/crypto */
	tn_b2s s;
	tn_b2s_init(&s, 16, gv_aead_key, 32);
	tn_b2s_update(&s, gv_aead_pt, 116);
	tn_b2s_final(&s, out);
	CHECK_MEM(out, gv_b2s128_keyed_116, 16);
	tn_hmac_b2s(out, (const uint8_t *)"hmac key", 8, gv_aead_pt, sizeof gv_aead_pt);
	CHECK_MEM(out, gv_hmac_b2s, 32);
}

static void kdf_tests(void)
{
	/* wireguard-go device/kdf_test.go TestKDF (KDF3) */
	static const struct { const char *key, *input, *t0, *t1, *t2; } v[] = {
		{"746573742d6b6579", "746573742d696e707574",
		 "6f0e5ad38daba1bea8a0d213688736f19763239305e0f58aba697f9ffc41c633",
		 "df1194df20802a4fe594cde27e92991c8cae66c366e8106aaa937a55fa371e8a",
		 "fac6e2745a325f5dc5d11a5b165aad08b0ada28e7b4e666b7c077934a4d76c24"},
		{"776972656775617264", "776972656775617264",
		 "491d43bbfdaa8750aaf535e334ecbfe5129967cd64635101c566d4caefda96e8",
		 "1e71a379baefd8a79aa4662212fcafe19a23e2b609a3db7d6bcba8f560e3d25f",
		 "31e1ae48bddfbe5de38f295e5452b1909a1b4e38e183926af3780b0c1e1f0160"},
		{"", "",
		 "8387b46bf43eccfcf349552a095d8315c4055beb90208fb1be23b894bc2ed5d0",
		 "58a0e5f6faefccf4807bff1f05fa8a9217945762040bcec2f4b4a62bdfe0e86e",
		 "0ce6ea98ec548f8e281e93e32db65621c45eb18dc6f0a7ad94178610a2f7338e"},
	};
	for (size_t i = 0; i < sizeof v / sizeof v[0]; i++) {
		uint8_t key[64], inp[64], t0[32], t1[32], t2[32], exp[32];
		size_t kl = tt_unhex(key, sizeof key, v[i].key);
		size_t il = tt_unhex(inp, sizeof inp, v[i].input);
		/* HKDF by hand from tn_hmac_b2s first */
		uint8_t prk[32], buf[33];
		tn_hmac_b2s(prk, key, kl, inp, il);
		buf[0] = 1;
		tn_hmac_b2s(t0, prk, 32, buf, 1);
		memcpy(buf, t0, 32);
		buf[32] = 2;
		tn_hmac_b2s(t1, prk, 32, buf, 33);
		memcpy(buf, t1, 32);
		buf[32] = 3;
		tn_hmac_b2s(t2, prk, 32, buf, 33);
		tt_unhex(exp, 32, v[i].t0);
		CHECK_MEM(t0, exp, 32);
		tt_unhex(exp, 32, v[i].t1);
		CHECK_MEM(t1, exp, 32);
		tt_unhex(exp, 32, v[i].t2);
		CHECK_MEM(t2, exp, 32);
		/* tn_kdf: HMAC zero-pads keys to 64 bytes, so a short key equals its
		 * zero-padded 32-byte chaining-key form. */
		uint8_t ck[32] = {0}, a[32], b[32], c[32];
		memcpy(ck, key, kl);
		tn_kdf(ck, inp, il, a, b, c);
		CHECK_MEM(a, t0, 32);
		CHECK_MEM(b, t1, 32);
		CHECK_MEM(c, t2, 32);
	}
}

static void salsa_tests(void)
{
	/* ECRYPT Salsa20 set 6 (from golang.org/x/crypto/salsa20/salsa20_test.go):
	 * XOR of all 64-byte blocks of 131072 keystream bytes. */
	static const struct { const char *key, *iv, *xor; } v[] = {
		{"0053A6F94C9FF24598EB3E91E4378ADD3083D6297CCF2275C81B6EC11467BA0D", "0D74DB42A91077DE",
		 "C349B6A51A3EC9B712EAED3F90D8BCEE69B7628645F251A996F55260C62EF31FD6C6B0AEA94E136C9D984AD2DF3578F78E457527B03A0450580DD874F63B1AB9"},
		{"0558ABFE51A4F74A9DF04396E93C8FE23588DB2E81D4277ACD2073C6196CBF12", "167DE44BB21980E7",
		 "C3EAAF32836BACE32D04E1124231EF47E101367D6305413A0EEB07C60698A2876E4D031870A739D6FFDDD208597AFF0A47AC17EDB0167DD67EBA84F1883D4DFD"},
		{"0A5DB00356A9FC4FA2F5489BEE4194E73A8DE03386D92C7FD22578CB1E71C417", "1F86ED54BB2289F0",
		 "3CD23C3DC90201ACC0CF49B440B6C417F0DC8D8410A716D5314C059E14B1A8D9A9FB8EA3D9C8DAE12B21402F674AA95C67B1FC514E994C9D3F3A6E41DFF5BBA6"},
		{"0F62B5085BAE0154A7FA4DA0F34699EC3F92E5388BDE3184D72A7DD02376C91C", "288FF65DC42B92F9",
		 "E00EBCCD70D69152725F9987982178A2E2E139C7BCBE04CA8A0E99E318D9AB76F988C8549F75ADD790BA4F81C176DA653C1A043F11A958E169B6D2319F4EEC1A"},
	};
	for (size_t i = 0; i < sizeof v / sizeof v[0]; i++) {
		uint8_t key[32], iv[8], exp[64], x[64] = {0};
		tt_unhex(key, 32, v[i].key);
		tt_unhex(iv, 8, v[i].iv);
		tt_unhex(exp, 64, v[i].xor);
		for (uint64_t blk = 0; blk < 131072 / 4096; blk++) {
			tn_salsa20_xor(A, NULL, 4096, iv, blk * 64, key);
			for (int k = 0; k < 4096; k++)
				x[k % 64] ^= A[k];
		}
		CHECK_MEM(x, exp, 64);
	}

	/* XSalsa20 (x/crypto salsa20_test.go TestXSalsa20) */
	const uint8_t *nonce = (const uint8_t *)"24-byte nonce for xsalsa";
	const uint8_t *key = (const uint8_t *)"this is 32-byte key for xsalsa20";
	tn_xsalsa20_xor(A, (const uint8_t *)"Hello world!", 12, nonce, key);
	tt_unhex(B, sizeof B, "002d4513843fc240c401e541");
	CHECK_MEM(A, B, 12);
	memset(C, 0, 64);
	tn_xsalsa20_xor(A, C, 64, nonce, key);
	tt_unhex(B, sizeof B,
		 "4848297feb1fb52fb66d81609bd547fabcbe7026edc8b5e5e449d088bfa69c088f5d8da1d791267c2c195a7f8cae9c4b4050d08ce6d3a151ec265f3a58e47648");
	CHECK_MEM(A, B, 64);
}

static void box_tests(void)
{
	/* x/crypto nacl/box TestBox: "expected was generated using the C implementation of NaCl". */
	uint8_t sk1[32], sk2[32], pk1[32], k[32], nonce[24], msg[64];
	memset(sk1, 1, 32);
	memset(sk2, 2, 32);
	memset(nonce, 4, 24);
	memset(msg, 3, 64);
	tn_x25519_public(pk1, sk1);
	CHECK(tn_box_beforenm(k, pk1, sk2) == 0);
	tn_secretbox_seal(A, msg, 64, nonce, k);
	tt_unhex(B, sizeof B,
		 "78ea30b19d2341ebbdba54180f821eec265cf86312549bea8a37652a8bb94f07b78a73ed1708085e6ddd0e943bbdeb87"
		 "55079a37eb31d86163ce241164a47629c0539f330b4914cd135b3855bc2a2dfc");
	CHECK_MEM(A, B, 80);
	CHECK(tn_secretbox_open(C, A, 80, nonce, k) == 0);
	CHECK_MEM(C, msg, 64);
	A[20] ^= 1;
	CHECK(tn_secretbox_open(C, A, 80, nonce, k) == -1);

	/* Go-derived: box.Seal and box.Precompute with other keys and an odd length */
	CHECK(tn_box_beforenm(k, gv_box_pk2, gv_box_sk1) == 0);
	CHECK_MEM(k, gv_box_shared, 32);
	uint8_t k2[32];
	CHECK(tn_box_beforenm(k2, gv_box_pk1, gv_box_sk2) == 0); /* symmetric */
	CHECK_MEM(k2, k, 32);
	tn_secretbox_seal(A, gv_box_msg, sizeof gv_box_msg, gv_box_nonce, k);
	CHECK(sizeof gv_box_sealed == sizeof gv_box_msg + 16);
	CHECK_MEM(A, gv_box_sealed, sizeof gv_box_sealed);
	CHECK(tn_secretbox_open(C, gv_box_sealed, sizeof gv_box_sealed, gv_box_nonce, k) == 0);
	CHECK_MEM(C, gv_box_msg, sizeof gv_box_msg);
	uint8_t sk[32];
	memset(sk, 0, 32);
	for (int i = 0; i < 32; i++)
		sk[i] = (uint8_t)(0x55 + i);
	tn_secretbox_seal(A, NULL, 0, gv_box_nonce, sk);
	CHECK_MEM(A, gv_secretbox_empty, 16);

	/* a low-order public key gives an all-zero DH: rejected */
	uint8_t zero_pk[32] = {0};
	CHECK(tn_box_beforenm(k, zero_pk, sk1) == -1);
}

static void x25519_aead_tests(void)
{
	/* RFC 7748 6.1 */
	uint8_t a[32], b[32], pa[32], pb[32], s1[32], s2[32], exp[32];
	tt_unhex(a, 32, "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
	tt_unhex(b, 32, "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
	tn_x25519_public(pa, a);
	tn_x25519_public(pb, b);
	tt_unhex(exp, 32, "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
	CHECK_MEM(pa, exp, 32);
	tt_unhex(exp, 32, "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
	CHECK_MEM(pb, exp, 32);
	CHECK(tn_x25519(s1, a, pb) == 0 && tn_x25519(s2, b, pa) == 0);
	tt_unhex(exp, 32, "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
	CHECK_MEM(s1, exp, 32);
	CHECK_MEM(s2, exp, 32);

	/* RFC 8439 2.8.2 AEAD_CHACHA20_POLY1305 */
	uint8_t key[32], nonce[12], aad[12];
	for (int i = 0; i < 32; i++)
		key[i] = (uint8_t)(0x80 + i);
	tt_unhex(nonce, 12, "070000004041424344454647");
	tt_unhex(aad, 12, "50515253c0c1c2c3c4c5c6c7");
	const char *pt = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the "
			 "future, sunscreen would be it.";
	size_t n = strlen(pt);
	tn_aead_seal(A, key, nonce, aad, 12, (const uint8_t *)pt, n);
	size_t en = tt_unhex(B, sizeof B,
			     "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b"
			     "1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc"
			     "3ff4def08e4b7a9de576d26586cec64b6116"
			     "1ae10b594f09e26a7e902ecbd0600691");
	CHECK(en == n + 16);
	CHECK_MEM(A, B, n + 16);
	CHECK(tn_aead_open(C, key, nonce, aad, 12, A, n + 16) == 0);
	CHECK_MEM(C, pt, n);
	A[0] ^= 1;
	CHECK(tn_aead_open(C, key, nonce, aad, 12, A, n + 16) == -1);

	/* Go-derived (x/crypto chacha20poly1305) with a big-endian counter nonce, as ts2021 uses */
	tn_aead_seal(A, gv_aead_key, gv_aead_nonce, gv_aead_ad, sizeof gv_aead_ad, gv_aead_pt, sizeof gv_aead_pt);
	CHECK_MEM(A, gv_aead_ct, sizeof gv_aead_ct);

	/* The Monocypher ratchet trap (spec 11): sealing the same input twice with our
	 * wrapper must give identical output (a reused crypto_aead_ctx would not). */
	tn_aead_seal(B, gv_aead_key, gv_aead_nonce, gv_aead_ad, sizeof gv_aead_ad, gv_aead_pt, sizeof gv_aead_pt);
	CHECK_MEM(A, B, sizeof gv_aead_ct);
}

static void key_text_tests(void)
{
	uint8_t priv[32], pub[32], back[32];
	char t[80];
	CHECK(tn_key_parse(priv, "privkey:", gv_mpriv_text, strlen(gv_mpriv_text)) == 0);
	tn_x25519_public(pub, priv);
	tn_key_text(t, "mkey:", pub);
	CHECK(strcmp(t, gv_mkey_text) == 0); /* matches key.MachinePrivate.Public().String() */
	CHECK(tn_key_parse(back, "mkey:", t, strlen(t)) == 0);
	CHECK_MEM(back, pub, 32);
	CHECK(tn_key_parse(back, "nodekey:", t, strlen(t)) == -1);
	tn_key_new(priv, pub);
	CHECK((priv[0] & 7) == 0 && (priv[31] & 0xc0) == 0x40); /* clamped */
}

int main(void)
{
	blake2s_tests();
	kdf_tests();
	salsa_tests();
	box_tests();
	x25519_aead_tests();
	key_text_tests();
	return tt_done("test_crypto");
}
