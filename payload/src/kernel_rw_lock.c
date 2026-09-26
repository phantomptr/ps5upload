/*
 * kernel_rw_lock.c — definition of the shared kernel-R/W serialization
 * mutex declared in kernel_rw_lock.h. Single translation unit so the
 * `extern` in the header has exactly one definition; PTHREAD_MUTEX_
 * INITIALIZER does the rest. See the header for the full rationale and
 * the innermost-lock ordering rule.
 */

#include "kernel_rw_lock.h"

pthread_mutex_t kernel_rw_lock = PTHREAD_MUTEX_INITIALIZER;
