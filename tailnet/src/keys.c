/* Machine/node/disco keys, their text forms, and the state file (spec 2). */
#include "tn.h"
#include "crypto.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define STATE_FILE "tailnet-state.json"
#define STATE_MAX 4096

/* Clamped like key.NewMachine / key.NewNode (tailscale types/key, BSD-3). */
void tn_key_new(uint8_t priv[32], uint8_t pub[32])
{
	tn_random(priv, 32);
	priv[0] &= 248;
	priv[31] = (priv[31] & 127) | 64;
	tn_x25519_public(pub, priv);
}

void tn_key_text(char out[80], const char *prefix, const uint8_t k[32])
{
	size_t p = strlen(prefix);
	memcpy(out, prefix, p);
	tn_hex(out + p, k, 32);
}

int tn_key_parse(uint8_t k[32], const char *prefix, const char *s, size_t n)
{
	size_t p = strlen(prefix);
	if (n != p + 64 || memcmp(s, prefix, p) != 0)
		return -1;
	return tn_unhex(k, 32, s + p, 64);
}

static int path_join(char *out, size_t cap, const char *dir, const char *name)
{
	int n = snprintf(out, cap, "%s/%s", dir, name);
	return n < 0 || (size_t)n >= cap ? -1 : 0;
}

static int get_key(const tn_json *j, const char *name, const char *prefix, uint8_t k[32])
{
	int t = tn_json_get(j, 0, name);
	if (tn_json_type(j, t) != TN_J_STR)
		return -1;
	return tn_key_parse(k, prefix, j->s + j->t[t].start, j->t[t].end - j->t[t].start);
}

int tn_state_load(tn_keys *k, const char *dir, const char *control_url)
{
	char path[600], buf[STATE_MAX + 1];
	tn_jtok toks[64];
	tn_json j = {.t = toks, .cap = 64};
	int dirty = 0;

	memset(k, 0, sizeof *k);
	if (mkdir(dir, 0700) != 0 && errno != EEXIST)
		return TN_E_STATE;
	if (path_join(path, sizeof path, dir, STATE_FILE) < 0)
		return TN_E_ARG;
	int fd = open(path, O_RDONLY);
	ssize_t n = 0;
	if (fd >= 0) {
		n = read(fd, buf, STATE_MAX);
		close(fd);
		if (n < 0)
			return TN_E_STATE;
	} else if (errno != ENOENT) {
		return TN_E_STATE;
	}
	buf[n > 0 ? n : 0] = 0;
	if (n > 0 && tn_json_parse(&j, buf, (size_t)n) > 0 && tn_json_type(&j, 0) == TN_J_OBJ) {
		if (get_key(&j, "machine_priv", "privkey:", k->machine_priv) == 0)
			tn_x25519_public(k->machine_pub, k->machine_priv);
		else
			dirty = 1;
		if (get_key(&j, "node_priv", "privkey:", k->node_priv) == 0)
			tn_x25519_public(k->node_pub, k->node_priv);
		else
			dirty = 1;
		k->has_old_node = get_key(&j, "old_node_pub", "nodekey:", k->old_node_pub) == 0;
		k->has_control_key = get_key(&j, "control_noise_key", "mkey:", k->control_key) == 0;
		tn_json_str(&j, tn_json_get(&j, 0, "control_url"), k->control_url, sizeof k->control_url);
		k->logged_in = tn_json_bool(&j, tn_json_get(&j, 0, "logged_in")) == 1;
		tn_json_i64(&j, tn_json_get(&j, 0, "node_id"), &k->node_id);
	} else {
		dirty = 1;
	}
	if (dirty) {
		if (!memcmp(k->machine_pub, (uint8_t[32]){0}, 32))
			tn_key_new(k->machine_priv, k->machine_pub);
		if (!memcmp(k->node_pub, (uint8_t[32]){0}, 32))
			tn_key_new(k->node_priv, k->node_pub);
	}
	if (strcmp(k->control_url, control_url) != 0) {
		/* Another control server: its key and our login there are unknown. */
		tn_strlcpy(k->control_url, control_url, sizeof k->control_url);
		k->has_control_key = 0;
		k->logged_in = 0;
		k->node_id = 0;
		dirty = 1;
	}
	tn_key_new(k->disco_priv, k->disco_pub);
	tn_wipe(buf, sizeof buf);
	return dirty ? tn_state_save(k, dir) : TN_OK;
}

int tn_state_save(const tn_keys *k, const char *dir)
{
	char path[600], tmp[620], buf[STATE_MAX], t[80];
	tn_jw w;
	tn_jw_init(&w, buf, sizeof buf);
	tn_jw_obj(&w);
	tn_jw_kint(&w, "v", 1);
	tn_jw_kstr(&w, "control_url", k->control_url);
	if (k->has_control_key) {
		tn_key_text(t, "mkey:", k->control_key);
		tn_jw_kstr(&w, "control_noise_key", t);
	}
	tn_key_text(t, "privkey:", k->machine_priv);
	tn_jw_kstr(&w, "machine_priv", t);
	tn_key_text(t, "privkey:", k->node_priv);
	tn_jw_kstr(&w, "node_priv", t);
	if (k->has_old_node) {
		tn_key_text(t, "nodekey:", k->old_node_pub);
		tn_jw_kstr(&w, "old_node_pub", t);
	}
	tn_jw_kint(&w, "node_id", k->node_id);
	tn_jw_kbool(&w, "logged_in", k->logged_in);
	tn_jw_end(&w);
	tn_jw_raw(&w, "\n");
	tn_wipe(t, sizeof t);
	if (w.err)
		return TN_E_TOOBIG;
	if (path_join(path, sizeof path, dir, STATE_FILE) < 0 ||
	    path_join(tmp, sizeof tmp, dir, STATE_FILE ".tmp") < 0)
		return TN_E_ARG;
	/* Atomic replace: temp file in the same directory, fsync, rename (never across devices). */
	int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
	if (fd < 0)
		return TN_E_STATE;
	size_t off = 0;
	while (off < w.len) {
		ssize_t r = write(fd, buf + off, w.len - off);
		if (r < 0) {
			if (errno == EINTR)
				continue;
			close(fd);
			unlink(tmp);
			return TN_E_STATE;
		}
		off += (size_t)r;
	}
	tn_wipe(buf, sizeof buf);
	if (fsync(fd) != 0 || close(fd) != 0) {
		unlink(tmp);
		return TN_E_STATE;
	}
	if (rename(tmp, path) != 0) {
		unlink(tmp);
		return TN_E_STATE;
	}
	return TN_OK;
}
