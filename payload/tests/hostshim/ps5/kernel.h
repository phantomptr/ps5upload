/* Host-build shim for <ps5/kernel.h>.
 *
 * The SDK header is not available off-console. activity.c includes it
 * but takes only sceKernelGetAppInfo from it, and app_info.h already
 * declares that -- so for the host selftest this can be empty. Keeping
 * the include path shimmed (rather than editing activity.c) means the
 * test compiles the real source, unmodified. */
#ifndef PS5UPLOAD_HOSTSHIM_PS5_KERNEL_H
#define PS5UPLOAD_HOSTSHIM_PS5_KERNEL_H
#endif
