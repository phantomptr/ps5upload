// Command vectors derives interop test vectors for the tailnet C library from
// Tailscale's own Go code (tailscale.com, BSD-3-Clause) and golang.org/x/{crypto,net}.
// It prints a C header on stdout; gen.sh writes it to tailnet/tests/vectors_go.h.
//
// Noise (ts2021): the initiation is built here from fixed keys (mirroring
// control/controlbase/handshake.go), then fed to the real controlbase.Server,
// whose response and first record are captured. The C test must reproduce the
// initiation byte for byte and finish the handshake on the captured response.
package main

import (
	"bytes"
	"context"
	"crypto/hmac"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"hash"
	"io"
	"net"
	"net/netip"
	"os"
	"strings"
	"time"

	"go4.org/mem"
	"golang.org/x/crypto/blake2s"
	"golang.org/x/crypto/chacha20poly1305"
	"golang.org/x/crypto/curve25519"
	"golang.org/x/crypto/nacl/box"
	"golang.org/x/crypto/nacl/secretbox"
	"golang.org/x/net/http2/hpack"
	"tailscale.com/control/controlbase"
	"tailscale.com/tailcfg"
	"tailscale.com/types/key"
)

const capver = 130

func fill(b byte) (k [32]byte) {
	for i := range k {
		k[i] = b + byte(i)
	}
	return
}

func cbytes(name string, b []byte) {
	fmt.Printf("static const unsigned char %s[%d] = {", name, len(b))
	for i, c := range b {
		if i%16 == 0 {
			fmt.Printf("\n\t")
		}
		fmt.Printf("0x%02x,", c)
	}
	fmt.Printf("\n};\n")
}

func cstr(name, s string) {
	fmt.Printf("static const char %s[] = %q;\n", name, s)
}

// --- a minimal Noise IK client mirroring controlbase/handshake.go ---

type sym struct{ h, ck [32]byte }

func newBlake() hash.Hash { h, _ := blake2s.New256(nil); return h }

func (s *sym) mixHash(d []byte) {
	h := newBlake()
	h.Write(s.h[:])
	h.Write(d)
	h.Sum(s.h[:0])
}

func hkdf2(ck, ikm []byte) (a, b [32]byte) {
	m := hmac.New(newBlake, ck)
	m.Write(ikm)
	prk := m.Sum(nil)
	m = hmac.New(newBlake, prk)
	m.Write([]byte{1})
	t1 := m.Sum(nil)
	m = hmac.New(newBlake, prk)
	m.Write(t1)
	m.Write([]byte{2})
	t2 := m.Sum(nil)
	copy(a[:], t1)
	copy(b[:], t2)
	return
}

func (s *sym) mixDH(priv, pub [32]byte) [32]byte {
	ss, err := curve25519.X25519(priv[:], pub[:])
	if err != nil {
		panic(err)
	}
	var k [32]byte
	s.ck, k = hkdf2(s.ck[:], ss)
	return k
}

func (s *sym) encryptAndHash(k [32]byte, pt []byte) []byte {
	a, _ := chacha20poly1305.New(k[:])
	ct := a.Seal(nil, make([]byte, 12), pt, s.h[:])
	s.mixHash(ct)
	return ct
}

func pub(priv [32]byte) [32]byte {
	p, _ := curve25519.X25519(priv[:], curve25519.Basepoint)
	var o [32]byte
	copy(o[:], p)
	return o
}

func privText(k [32]byte) string { return "privkey:" + hex.EncodeToString(k[:]) }

func noiseVectors() {
	machinePriv := fill(0x10)
	controlPriv := fill(0x40)
	ephPriv := fill(0x70)
	var mk key.MachinePrivate
	var ck key.MachinePrivate
	if err := mk.UnmarshalText([]byte(privText(machinePriv))); err != nil {
		panic(err)
	}
	if err := ck.UnmarshalText([]byte(privText(controlPriv))); err != nil {
		panic(err)
	}
	controlPub := pub(controlPriv)
	machinePub := pub(machinePriv)
	ephPub := pub(ephPriv)
	if mk.Public().String() != "mkey:"+hex.EncodeToString(machinePub[:]) {
		panic("machine pub mismatch")
	}

	var s sym
	s.h = blake2s.Sum256([]byte("Noise_IK_25519_ChaChaPoly_BLAKE2s"))
	s.ck = s.h
	s.mixHash([]byte(fmt.Sprintf("Tailscale Control Protocol v%d", capver)))
	s.mixHash(controlPub[:])
	s.mixHash(ephPub[:])
	k := s.mixDH(ephPriv, controlPub)
	encS := s.encryptAndHash(k, machinePub[:])
	k = s.mixDH(machinePriv, controlPub)
	tag := s.encryptAndHash(k, nil)
	init := make([]byte, 0, 101)
	init = binary.BigEndian.AppendUint16(init, capver)
	init = append(init, 1)
	init = binary.BigEndian.AppendUint16(init, 96)
	init = append(init, ephPub[:]...)
	init = append(init, encS...)
	init = append(init, tag...)

	// Run the real server on a pipe.
	cconn, sconn := net.Pipe()
	type res struct {
		c   *controlbase.Conn
		err error
	}
	done := make(chan res, 1)
	go func() {
		c, err := controlbase.Server(context.Background(), sconn, ck, nil)
		done <- res{c, err}
	}()
	go func() { cconn.Write(init) }()
	resp := make([]byte, 51)
	if _, err := io.ReadFull(cconn, resp); err != nil {
		panic(err)
	}
	r := <-done
	if r.err != nil {
		panic(r.err)
	}
	if r.c.Peer() != mk.Public() {
		panic("server saw wrong machine key")
	}
	serverMsg := []byte("hello from the Go controlbase server")
	go func() { r.c.Write(serverMsg) }()
	rec := make([]byte, 3+len(serverMsg)+16)
	if _, err := io.ReadFull(cconn, rec); err != nil {
		panic(err)
	}

	// Finish on our side to derive the client->server key and a client record.
	var re [32]byte
	copy(re[:], resp[3:35])
	s.mixHash(re[:])
	s.mixDH(ephPriv, re)
	k = s.mixDH(machinePriv, re)
	a, _ := chacha20poly1305.New(k[:])
	if _, err := a.Open(nil, make([]byte, 12), resp[35:51], s.h[:]); err != nil {
		panic(err)
	}
	s.mixHash(resp[35:51])
	k1, k2 := hkdf2(s.ck[:], nil)
	c1, _ := chacha20poly1305.New(k1[:])
	c2, _ := chacha20poly1305.New(k2[:])
	if pt, err := c2.Open(nil, make([]byte, 12), rec[3:], nil); err != nil || !bytes.Equal(pt, serverMsg) {
		panic("server record did not decrypt")
	}
	clientMsg := []byte("hello from the C client")
	var n1 [12]byte
	binary.BigEndian.PutUint64(n1[4:], 1) // second record: counter 1
	ct0 := c1.Seal(nil, make([]byte, 12), []byte("x"), nil)
	ct1 := c1.Seal(nil, n1[:], clientMsg, nil)
	crec := []byte{4, 0, byte(len(ct1))}
	crec = append(crec, ct1...)
	go func() {
		cconn.Write(append([]byte{4, 0, byte(len(ct0))}, ct0...))
		cconn.Write(crec)
	}()
	buf := make([]byte, 64)
	n, _ := io.ReadFull(r.c, buf[:1])
	n2, err := io.ReadFull(r.c, buf[:len(clientMsg)])
	if err != nil || n != 1 || !bytes.Equal(buf[:n2], clientMsg) {
		panic("server did not read our records")
	}

	fmt.Printf("/* ts2021 Noise IK: initiation built from fixed keys and accepted by\n" +
		" * tailscale.com/control/controlbase.Server; response and records captured from it. */\n")
	fmt.Printf("#define GV_NOISE_CAPVER %d\n", capver)
	cbytes("gv_noise_machine_priv", machinePriv[:])
	cbytes("gv_noise_control_priv", controlPriv[:])
	cbytes("gv_noise_control_pub", controlPub[:])
	cbytes("gv_noise_eph_priv", ephPriv[:])
	cbytes("gv_noise_init", init)
	cbytes("gv_noise_resp", resp)
	cbytes("gv_noise_server_rec", rec)
	cstr("gv_noise_server_msg", string(serverMsg))
	cstr("gv_noise_client_msg", string(clientMsg))
	cbytes("gv_noise_client_rec1", crec)
	cbytes("gv_noise_k1", k1[:])
	cbytes("gv_noise_k2", k2[:])
	cstr("gv_mkey_text", mk.Public().String())
	cstr("gv_mpriv_text", privText(machinePriv))
}

func cryptoVectors() {
	// NaCl box (x/crypto/nacl/box) with fixed keys.
	sk1 := fill(0x01)
	sk2 := fill(0x21)
	pk1, pk2 := pub(sk1), pub(sk2)
	var nonce [24]byte
	copy(nonce[:], "24-byte nonce for disco!")
	msg := []byte("disco ping body, any length: 0123456789abcdef0123456789")
	sealed := box.Seal(nil, msg, &nonce, &pk2, &sk1)
	var shared [32]byte
	box.Precompute(&shared, &pk2, &sk1)
	fmt.Printf("/* golang.org/x/crypto/nacl/box */\n")
	cbytes("gv_box_sk1", sk1[:])
	cbytes("gv_box_pk1", pk1[:])
	cbytes("gv_box_sk2", sk2[:])
	cbytes("gv_box_pk2", pk2[:])
	cbytes("gv_box_nonce", nonce[:])
	cbytes("gv_box_msg", msg)
	cbytes("gv_box_sealed", sealed)
	cbytes("gv_box_shared", shared[:])
	var skey [32]byte = fill(0x55)
	sb := secretbox.Seal(nil, []byte{}, &nonce, &skey)
	cbytes("gv_secretbox_empty", sb)

	// ChaCha20-Poly1305 IETF with AD and a non-zero nonce.
	kk := fill(0x80)
	a, _ := chacha20poly1305.New(kk[:])
	var n [12]byte
	binary.BigEndian.PutUint64(n[4:], 0x0102030405060708)
	pt := bytes.Repeat([]byte("ts2021 "), 37)
	ad := []byte("additional data")
	fmt.Printf("/* golang.org/x/crypto/chacha20poly1305 */\n")
	cbytes("gv_aead_key", kk[:])
	cbytes("gv_aead_nonce", n[:])
	cbytes("gv_aead_ad", ad)
	cbytes("gv_aead_pt", pt)
	cbytes("gv_aead_ct", a.Seal(nil, n[:], pt, ad))

	// Keyed BLAKE2s-128 (WireGuard MAC1 shape) and HMAC-BLAKE2s.
	h, _ := blake2s.New128(kk[:])
	h.Write(pt[:116])
	fmt.Printf("/* golang.org/x/crypto/blake2s */\n")
	cbytes("gv_b2s128_keyed_116", h.Sum(nil))
	m := hmac.New(newBlake, []byte("hmac key"))
	m.Write(pt)
	cbytes("gv_hmac_b2s", m.Sum(nil))
}

func hpackVectors() {
	// Go's encoder: Huffman + dynamic table (as Go's HTTP/2 server sends).
	var buf bytes.Buffer
	e := hpack.NewEncoder(&buf)
	blocks := [][]hpack.HeaderField{
		{{Name: ":status", Value: "200"}, {Name: "content-type", Value: "application/json; charset=utf-8"}, {Name: "date", Value: "Sat, 07 Nov 2026 22:00:00 GMT"}},
		{{Name: ":status", Value: "200"}, {Name: "content-type", Value: "application/json; charset=utf-8"}, {Name: "date", Value: "Sat, 07 Nov 2026 22:00:01 GMT"}, {Name: "x-custom", Value: strings.Repeat("v", 70)}},
		{{Name: ":status", Value: "404"}, {Name: "content-length", Value: "19"}, {Name: "x-custom", Value: strings.Repeat("v", 70)}},
	}
	fmt.Printf("/* golang.org/x/net/http2/hpack encoder (Huffman + dynamic table) */\n")
	fmt.Printf("#define GV_HPACK_N %d\n", len(blocks))
	var expect []string
	for i, b := range blocks {
		buf.Reset()
		for _, f := range b {
			e.WriteField(f)
		}
		cbytes(fmt.Sprintf("gv_hpack_%d", i), buf.Bytes())
		var parts []string
		for _, f := range b {
			parts = append(parts, f.Name+": "+f.Value)
		}
		expect = append(expect, strings.Join(parts, "\n")+"\n")
	}
	fmt.Printf("static const unsigned char *const gv_hpack_blocks[] = {")
	for i := range blocks {
		fmt.Printf("gv_hpack_%d,", i)
	}
	fmt.Printf("};\nstatic const unsigned gv_hpack_lens[] = {")
	for i := range blocks {
		fmt.Printf("sizeof gv_hpack_%d,", i)
	}
	fmt.Printf("};\nstatic const char *const gv_hpack_expect[] = {\n")
	for _, s := range expect {
		fmt.Printf("\t%q,\n", s)
	}
	fmt.Printf("};\n")
}

func mapFixture() {
	// A delta MapResponse marshaled by tailcfg itself: PeersChanged,
	// PeersRemoved, PeersChangedPatch, PacketFilters, ControlTime.
	var np key.NodePublic
	if err := np.UnmarshalText([]byte("nodekey:" + strings.Repeat("ab", 32))); err != nil {
		panic(err)
	}
	dr := mustRaw(strings.Repeat("cd", 32))
	dk := key.DiscoPublicFromRaw32(mem.B(dr[:]))
	online := true
	exp := time.Date(2027, 1, 2, 3, 4, 5, 0, time.UTC)
	ct := time.Date(2026, 11, 7, 12, 0, 0, 0, time.UTC)
	mr := tailcfg.MapResponse{
		ControlTime: &ct,
		PeersChanged: []*tailcfg.Node{{
			ID: 7, Name: "phone.tnbed.internal.", Key: np, DiscoKey: dk,
			Addresses:  []netip.Prefix{netip.MustParsePrefix("100.64.0.7/32"), netip.MustParsePrefix("fd7a:115c:a1e0::7/128")},
			AllowedIPs: []netip.Prefix{netip.MustParsePrefix("100.64.0.7/32")},
			Endpoints:  []netip.AddrPort{netip.MustParseAddrPort("192.168.1.20:41641"), netip.MustParseAddrPort("[2001:db8::1]:41641")},
			HomeDERP:   2, Online: &online, KeyExpiry: exp,
		}},
		PeersRemoved: []tailcfg.NodeID{3},
		PeersChangedPatch: []*tailcfg.PeerChange{{
			NodeID: 1, DERPRegion: 9,
			Endpoints: []netip.AddrPort{netip.MustParseAddrPort("203.0.113.9:12345")},
		}},
		PacketFilters: map[string][]tailcfg.FilterRule{"base": {{
			SrcIPs:   []string{"100.64.0.0/10", "100.64.0.7-100.64.0.9", "fd7a:115c:a1e0::/48"},
			DstPorts: []tailcfg.NetPortRange{{IP: "*", Ports: tailcfg.PortRange{First: 9295, Last: 9296}}},
			IPProto:  []int{6, 17},
		}}},
	}
	j, err := json.Marshal(mr)
	if err != nil {
		panic(err)
	}
	fmt.Printf("/* tailcfg.MapResponse delta marshaled by encoding/json */\n")
	cstr("gv_map_delta", string(j))
}

func mustRaw(h string) [32]byte {
	b, _ := hex.DecodeString(h)
	var o [32]byte
	copy(o[:], b)
	return o
}

func main() {
	fmt.Printf("/* Generated by tailnet/testbed/vectors (go run); do not edit. */\n")
	fmt.Printf("/* Sources: tailscale.com (BSD-3-Clause), golang.org/x/crypto, golang.org/x/net. */\n")
	noiseVectors()
	cryptoVectors()
	hpackVectors()
	mapFixture()
	os.Stdout.Sync()
}
