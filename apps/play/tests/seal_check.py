#!/usr/bin/env python3
# PS5Upload Play - opens the app's sealed knock (src/net/seal.hpp) with standard libraries, the way
# the host engine does (ECDH P-256, HKDF-SHA256, AES-128-GCM): the vector test_rooms writes.
# Usage: seal_check.py VECTOR_FILE  (lines: room private key, challenge, blob; base64url)
# SPDX-License-Identifier: GPL-3.0-or-later
import base64
import json
import sys


def b64(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def main():
    try:
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        from cryptography.hazmat.primitives.kdf.hkdf import HKDF
    except ImportError:
        print("seal: skipped (no python cryptography here)")
        return 0
    sk_b, challenge, blob = open(sys.argv[1]).read().split()
    box = b64(blob)
    assert box[0] == 1, "version byte"
    epk, nonce, ct = box[1:66], box[66:78], box[78:]
    sk = ec.derive_private_key(int.from_bytes(b64(sk_b), "big"), ec.SECP256R1())
    shared = sk.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), epk))
    key = HKDF(algorithm=hashes.SHA256(), length=16, salt=b64(challenge), info=b"ps5upload room knock v1").derive(shared)
    plain = json.loads(AESGCM(key).decrypt(nonce, ct, None))
    assert plain == {"secret": "pw", "gid": "g7", "challenge": challenge}, plain
    print("seal: the sealed knock opens with standard libraries")
    return 0


if __name__ == "__main__":
    sys.exit(main())
