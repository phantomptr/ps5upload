# sdk-crt

The startup code (`crt/`) of [ps5-payload-sdk](https://github.com/ps5-payload-dev/sdk) v0.43,
GPLv3 (see `LICENSE`), built by `payload/sdk-crt.mk` and linked into the helper, the installer
daemon and the patched elfldr in place of the SDK's own `crt1.o`.

One change, in `ps5upload.patch`: `__kernel_init` no longer returns `-ENOSYS` when the DMAP base
or the IOMMU softc search (added upstream in 4b86c73a) comes up empty. That return stopped every
payload before `main()` on FW 5.50, where 5.x builds (SDK v0.42, without the searches) ran.
Only two fallbacks use those values, and both refuse without them: the IOMMU write helpers
(which check for a missing softc themselves) and the physical-address path of
`kernel_proc_copyin`/`kernel_proc_copyout` (a guard added in `kernel_procio.c`).

To update: copy the new release's `crt/*.{c,h,S}` here, re-apply `ps5upload.patch`, and check
that the exported symbols still match the SDK's `target/lib/crt1.o` (`llvm-nm`).
