/*
 * sony_api_lock.c — definition of the shared Sony-API serialization
 * mutex declared in sony_api_lock.h. Single translation unit so the
 * `extern` in the header has exactly one definition; PTHREAD_MUTEX_
 * INITIALIZER does the rest.
 */

#include "sony_api_lock.h"

pthread_mutex_t sony_api_lock = PTHREAD_MUTEX_INITIALIZER;
