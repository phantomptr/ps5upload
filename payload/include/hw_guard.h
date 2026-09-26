#ifndef HW_GUARD_H
#define HW_GUARD_H

/*
 * hw_guard — fault containment for Sony hardware getters.
 *
 * The Hardware tab is the only screen that calls Sony hw getters
 * (sceKernelGetHwModelName / ...SerialNumber / sensor reads) via
 * dlsym(RTLD_DEFAULT). Those symbols resolve against whatever libraries are
 * loaded IN THE HOST PROCESS the payload was injected into — which differs
 * by ELF loader / autoloader. A getter that resolves-and-works under one
 * loader can FAULT (SIGSEGV/SIGBUS) under another, and a fault in any thread
 * kills the whole payload → the mgmt listener closes → the desktop sees
 * "connection refused" the instant the user opens Hardware (field report:
 * FW 9.60 via an autoloader+elfldr, deterministic; not reproducible via our
 * own :9021 loader).
 *
 * The guard arms a per-thread setjmp point around each risky call. The
 * process fatal-signal handler (main.c) calls hw_guard_try_recover() FIRST;
 * if the faulting thread was inside a guarded call it logs a breadcrumb and
 * siglongjmp's back so the field reports "unavailable" instead of dropping
 * the helper. Outside a guarded call it returns 0 and normal fatal cleanup
 * runs (unchanged behaviour for every other crash).
 */

/*
 * Called FIRST from the process fatal-signal handler with the delivered
 * signal. If the current thread is inside a guarded hw call AND `sig` is a
 * memory-fault signal (SIGSEGV/SIGBUS/SIGILL), this writes an
 * async-signal-safe breadcrumb naming the faulting getter and siglongjmp's
 * back to the guard — it does NOT return in that case. Otherwise it returns
 * 0 so the caller proceeds with its normal fatal-signal handling.
 */
int hw_guard_try_recover(int sig);

#endif /* HW_GUARD_H */
