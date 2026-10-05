/* Included by runtime.c after the management handlers: wrappers and the table built from
 * mgmt_table.def, the per-call environment, and runtime_mgmt_install(). */

#define MGMT_H0(m, f, a, fl, h, helper)                                                                    \
    static int mgmt_l_##m(void *st, int fd, uint64_t t, const char *b, uint64_t l) {                       \
        (void)fd;                                                                                          \
        (void)t;                                                                                           \
        (void)b;                                                                                           \
        (void)l;                                                                                           \
        return h((runtime_state_t *)st);                                                            \
    }                                                                                                      \
    static int mgmt_r_##m(const uint8_t *q, uint32_t n, mgmt_ctx_t *cx) { return helper(q, n, cx, mgmt_l_##m); }
#define MGMT_H1(m, f, a, fl, h, helper)                                                                    \
    static int mgmt_l_##m(void *st, int fd, uint64_t t, const char *b, uint64_t l) {                       \
        (void)fd;                                                                                          \
        (void)t;                                                                                           \
        return h((runtime_state_t *)st, b, l);                                                      \
    }                                                                                                      \
    static int mgmt_r_##m(const uint8_t *q, uint32_t n, mgmt_ctx_t *cx) { return helper(q, n, cx, mgmt_l_##m); }
#define MGMT_N(m, f, a, fl, run) /* native: the runner is the table's own */
/* job.run operations: the handler adapted to mgmt_legacy_fn. */
#define MGMT_OP0(op, f, a, fl, h)                                                                          \
    static int mgmt_lo_##op(void *st, int fd, uint64_t t, const char *b, uint64_t l) {                     \
        (void)fd;                                                                                          \
        (void)t;                                                                                           \
        (void)b;                                                                                           \
        (void)l;                                                                                           \
        return h((runtime_state_t *)st);                                                            \
    }
#define MGMT_OP1(op, f, a, fl, h)                                                                          \
    static int mgmt_lo_##op(void *st, int fd, uint64_t t, const char *b, uint64_t l) {                     \
        (void)fd;                                                                                          \
        (void)t;                                                                                           \
        return h((runtime_state_t *)st, b, l);                                                      \
    }
#define MGMT_OP1B(op, f, a, fl, h)                                                                         \
    static int mgmt_lo_##op(void *st, int fd, uint64_t t, const char *b, uint64_t l) {                     \
        (void)fd;                                                                                          \
        (void)t;                                                                                           \
        (void)l;                                                                                           \
        return h((runtime_state_t *)st, b);                                                         \
    }
#include "mgmt_table.def"
#undef MGMT_H0
#undef MGMT_H1
#undef MGMT_N
#undef MGMT_OP0
#undef MGMT_OP1
#undef MGMT_OP1B

#define MGMT_OP0(op, f, a, fl, h) /* the operations are tabled below */
#define MGMT_OP1(op, f, a, fl, h)
#define MGMT_OP1B(op, f, a, fl, h)
#define MGMT_H0(m, f, a, fl, h, helper) {(uint16_t)(m), (uint16_t)(f), (uint16_t)(a), (fl), mgmt_r_##m},
#define MGMT_H1(m, f, a, fl, h, helper) {(uint16_t)(m), (uint16_t)(f), (uint16_t)(a), (fl), mgmt_r_##m},
#define MGMT_N(m, f, a, fl, run) {(uint16_t)(m), (uint16_t)(f), (uint16_t)(a), (fl), run},
static const mgmt_entry_t k_mgmt_table[] = {
#include "mgmt_table.def"
};
#undef MGMT_H0
#undef MGMT_H1
#undef MGMT_N
#undef MGMT_OP0
#undef MGMT_OP1
#undef MGMT_OP1B

#define MGMT_H0(m, f, a, fl, h, helper)
#define MGMT_H1(m, f, a, fl, h, helper)
#define MGMT_N(m, f, a, fl, run)
#define MGMT_OP0(op, f, a, fl, h) {(uint8_t)(op), (uint16_t)(f), (uint16_t)(a), (fl), mgmt_lo_##op},
#define MGMT_OP1(op, f, a, fl, h) {(uint8_t)(op), (uint16_t)(f), (uint16_t)(a), (fl), mgmt_lo_##op},
#define MGMT_OP1B(op, f, a, fl, h) {(uint8_t)(op), (uint16_t)(f), (uint16_t)(a), (fl), mgmt_lo_##op},
static const mgmt_op_entry_t k_mgmt_ops[] = {
#include "mgmt_table.def"
};
#undef MGMT_H0
#undef MGMT_H1
#undef MGMT_N
#undef MGMT_OP0
#undef MGMT_OP1
#undef MGMT_OP1B

/* What the old management thread did around every frame:
 * the crash breadcrumb names the operation and the credentials are re-elevated (the first
 * attempt may predate kstuff). It does not move the command counter: the handlers bump it themselves
 * on success (as they did under the old protocol), and a second count here would double it. */
static void mgmt_env_enter(uint16_t legacy_frame) {
    g_inflight_frame_type = (unsigned int)legacy_frame;
    runtime_apply_ucred_jailbreak();
}

static void mgmt_env_leave(void) { g_inflight_frame_type = 0; }

/* The native filesystem methods (mgmt_fs.c) get their policy from here: the same allowlist, the
 * same read carve-outs and the same command counter the management handlers use. */
/* (the policy callbacks, and the counter the fs runners call, are defined in runtime.c above this include) */
int runtime_mgmt_install(runtime_state_t *state) {
    mgmt_fs_policy_t pol;
    int rc;
    memset(&pol, 0, sizeof pol);
    pol.write_allowed = is_path_allowed;
    pol.read_allowed = mgmt_fs_read_allowed;
    pol.count = mgmt_fs_count;
    pol.dev_of = NULL;     /* stat(2) */
    pol.src_dev_of = NULL; /* lstat(2) */
    g_mgmt_fs_state = state;
    mgmt_fs_set_policy(&pol);
    rc = mgmt_rpc_install(k_mgmt_table, sizeof k_mgmt_table / sizeof k_mgmt_table[0], state, mgmt_env_enter,
                              mgmt_env_leave);
    if (rc != 0) return rc;
    /* job.run operations that wrap a management handler (the rest: fs_jobs.c, registered by ava1_glue.c). */
    return mgmt_rpc_install_ops(k_mgmt_ops, sizeof k_mgmt_ops / sizeof k_mgmt_ops[0]);
}
