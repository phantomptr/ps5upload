/* PS5Upload Play - the TLS trust anchors: the Let's Encrypt (ISRG) roots the helper's tailnet
 * also uses (tailnet/src/isrg_roots.h, made with BearSSL's "brssl ta"). The hub's certificate
 * (hub.phantomptr.com) chains to one of them.
 * SPDX-License-Identifier: GPL-3.0-or-later */
#include "isrg_roots.h"

const br_x509_trust_anchor *play_tls_roots(size_t *n)
{
	*n = TN_ISRG_TAS_NUM;
	return tn_isrg_tas;
}
