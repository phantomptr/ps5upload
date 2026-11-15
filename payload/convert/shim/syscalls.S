/* PS5 syscall trampolines with POSIX -1/errno translation.
 * Numbers: ps5-payload-dev/sdk v0.43 for <512, with PS5-specific numbers
 * verified against McCaulay/mast1c0re sdk/include/ps/syscall/table.hpp.
 * Raw kernel error numbers must never be mistaken for successful fds/stat.
 */
.text
.macro PS5_CHECKED name, number
.section .text.\name,"ax",@progbits
.p2align 4
.globl \name
.type \name,@function
\name:
    mov $\number, %rax
    mov %rcx, %r10
    syscall
    jnc 1f
    mov %eax, %edi
    jmp ps5_syscall_error
1:  ret
.size \name, .-\name
.endm

PS5_CHECKED __wrap_accept4, 686
PS5_CHECKED __wrap_chown, 16
PS5_CHECKED __wrap_chroot, 61
PS5_CHECKED __wrap_dup, 41
PS5_CHECKED __wrap_faccessat, 489
PS5_CHECKED __wrap_fchdir, 13
PS5_CHECKED __wrap_fchmodat, 490
PS5_CHECKED __wrap_fchown, 123
PS5_CHECKED __wrap_fchownat, 491
PS5_CHECKED __wrap_fexecve, 492
PS5_CHECKED __wrap_fpathconf, 192
PS5_CHECKED __wrap_fstatat, 493
PS5_CHECKED __wrap_fstatfs, 397
PS5_CHECKED __wrap_futimens, 691
PS5_CHECKED __wrap_getpgid, 207
PS5_CHECKED __wrap_lchown, 254
PS5_CHECKED __wrap_link, 9
PS5_CHECKED __wrap_linkat, 495
PS5_CHECKED __wrap_minherit, 250
PS5_CHECKED __wrap_mkdirat, 496
PS5_CHECKED __wrap_mkfifo, 132
PS5_CHECKED __wrap_mknodat, 498
PS5_CHECKED __wrap_openat, 499
PS5_CHECKED __wrap_posix_openpt, 504
PS5_CHECKED __wrap_ppoll, 690
PS5_CHECKED __wrap_procctl, 689
PS5_CHECKED __wrap_readlink, 58
PS5_CHECKED __wrap_readlinkat, 500
PS5_CHECKED __wrap_renameat, 501
PS5_CHECKED __wrap_setgid, 181
PS5_CHECKED __wrap_setpgid, 82
PS5_CHECKED __wrap_setsid, 147
PS5_CHECKED __wrap_symlink, 57
PS5_CHECKED __wrap_symlinkat, 502
PS5_CHECKED __wrap_umask, 60
PS5_CHECKED __wrap_unlinkat, 503
PS5_CHECKED __wrap_utimensat, 692
PS5_CHECKED ps5_raw_fork, 2
.section .note.GNU-stack,"",@progbits
