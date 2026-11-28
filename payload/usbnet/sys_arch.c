/* SPDX-License-Identifier: GPL-3.0-or-later
 * Adapted from tom-ps5-usbnet (https://github.com/wujiezero/tom-ps5-usbnet, GPL-3.0-or-later),
 * port/sys_arch.c, for PS5Upload's USB adapter daemon (payload/usbnet/README.md). */
#define _DARWIN_C_SOURCE 1
#define _POSIX_C_SOURCE 200809L
#include "lwip/opt.h"
#include "lwip/sys.h"
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

struct un_sys_sem { pthread_mutex_t lock; pthread_cond_t changed; unsigned count; };
struct un_sys_mbox {
    pthread_mutex_t lock;
    pthread_cond_t readable, writable;
    void **messages;
    size_t size, head, count;
};
static pthread_mutex_t protect_lock;
static pthread_once_t protect_once = PTHREAD_ONCE_INIT;
static void protect_init(void)
{
    pthread_mutexattr_t attr;
    pthread_mutexattr_init(&attr);
    pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&protect_lock, &attr);
    pthread_mutexattr_destroy(&attr);
}
static void cond_init(pthread_cond_t *cond)
{
#ifdef UN_COND_REALTIME
    /* The PS5 build: the helper, whose imports are known to resolve, makes no condattr calls. */
    pthread_cond_init(cond, NULL);
#else
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
#ifndef __APPLE__
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
#endif
    pthread_cond_init(cond, &attr);
    pthread_condattr_destroy(&attr);
#endif
}
static int cond_wait(pthread_cond_t *cond, pthread_mutex_t *lock, u32_t remaining)
{
    struct timespec when;
#ifdef __APPLE__
    when.tv_sec = remaining / 1000;
    when.tv_nsec = (long)(remaining % 1000) * 1000000;
    return pthread_cond_timedwait_relative_np(cond, lock, &when);
#else
#ifdef UN_COND_REALTIME
    clock_gettime(CLOCK_REALTIME, &when);
#else
    clock_gettime(CLOCK_MONOTONIC, &when);
#endif
    when.tv_sec += remaining / 1000;
    when.tv_nsec += (long)(remaining % 1000) * 1000000;
    if (when.tv_nsec >= 1000000000L) { when.tv_sec++; when.tv_nsec -= 1000000000L; }
    return pthread_cond_timedwait(cond, lock, &when);
#endif
}
static pthread_key_t sem_key;
static pthread_once_t sem_key_once = PTHREAD_ONCE_INIT;
static void sem_key_destroy(void *value)
{
    sys_sem_t *slot = value;
    if (!slot) return;
    if (sys_sem_valid(slot)) sys_sem_free(slot);
    free(slot);
}
static void sem_key_init(void)
{
    pthread_key_create(&sem_key, sem_key_destroy);
}
static sys_sem_t *thread_sem_slot(void)
{
    sys_sem_t *slot;
    pthread_once(&sem_key_once, sem_key_init);
    slot = pthread_getspecific(sem_key);
    if (slot) return slot;
    slot = calloc(1, sizeof(*slot));
    if (!slot) return NULL;
    if (sys_sem_new(slot, 0) != ERR_OK || pthread_setspecific(sem_key, slot) != 0) {
        if (sys_sem_valid(slot)) sys_sem_free(slot);
        free(slot);
        return NULL;
    }
    return slot;
}
sys_sem_t *un_thread_sem_get(void)
{
    return thread_sem_slot();
}
void un_thread_sem_alloc(void)
{
    (void)thread_sem_slot();
}
void un_thread_sem_free(void)
{
    sys_sem_t *slot;
    pthread_once(&sem_key_once, sem_key_init);
    slot = pthread_getspecific(sem_key);
    if (!slot) return;
    pthread_setspecific(sem_key, NULL);
    sem_key_destroy(slot);
}
void sys_init(void) { pthread_once(&protect_once, protect_init); }
u32_t sys_now(void)
{
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (u32_t)((uint64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000);
}
u32_t sys_jiffies(void) { return sys_now(); }
void sys_msleep(u32_t ms)
{
    struct timespec delay = { ms / 1000, (long)(ms % 1000) * 1000000 };
    while (nanosleep(&delay, &delay) < 0 && errno == EINTR) {}
}
sys_prot_t sys_arch_protect(void)
{
    pthread_once(&protect_once, protect_init);
    pthread_mutex_lock(&protect_lock);
    return 0;
}
void sys_arch_unprotect(sys_prot_t token) { (void)token; pthread_mutex_unlock(&protect_lock); }
err_t sys_sem_new(sys_sem_t *out, u8_t initial)
{
    struct un_sys_sem *sem = calloc(1, sizeof(*sem));
    if (!sem) return ERR_MEM;
    pthread_mutex_init(&sem->lock, NULL);
    cond_init(&sem->changed);
    sem->count = initial;
    *out = sem;
    return ERR_OK;
}
void sys_sem_free(sys_sem_t *sem)
{
    pthread_cond_destroy(&(*sem)->changed);
    pthread_mutex_destroy(&(*sem)->lock);
    free(*sem); *sem = NULL;
}
void sys_sem_signal(sys_sem_t *sem)
{
    pthread_mutex_lock(&(*sem)->lock);
    (*sem)->count++;
    pthread_cond_signal(&(*sem)->changed);
    pthread_mutex_unlock(&(*sem)->lock);
}
u32_t sys_arch_sem_wait(sys_sem_t *sem, u32_t timeout)
{
    u32_t start = sys_now();
    pthread_mutex_lock(&(*sem)->lock);
    while (!(*sem)->count) {
        u32_t elapsed = sys_now() - start;
        if (timeout && elapsed >= timeout) {
            pthread_mutex_unlock(&(*sem)->lock); return SYS_ARCH_TIMEOUT;
        }
        if (timeout) cond_wait(&(*sem)->changed, &(*sem)->lock, timeout - elapsed);
        else pthread_cond_wait(&(*sem)->changed, &(*sem)->lock);
    }
    (*sem)->count--;
    pthread_mutex_unlock(&(*sem)->lock);
    return sys_now() - start;
}
err_t sys_mutex_new(sys_mutex_t *out)
{
    *out = malloc(sizeof(**out));
    if (!*out) return ERR_MEM;
    if (pthread_mutex_init(*out, NULL)) { free(*out); *out = NULL; return ERR_MEM; }
    return ERR_OK;
}
void sys_mutex_free(sys_mutex_t *mutex) { pthread_mutex_destroy(*mutex); free(*mutex); *mutex = NULL; }
void sys_mutex_lock(sys_mutex_t *mutex) { pthread_mutex_lock(*mutex); }
void sys_mutex_unlock(sys_mutex_t *mutex) { pthread_mutex_unlock(*mutex); }
err_t sys_mbox_new(sys_mbox_t *out, int size)
{
    struct un_sys_mbox *box = calloc(1, sizeof(*box));
    if (!box) return ERR_MEM;
    box->size = size > 0 ? (size_t)size : 128;
    box->messages = calloc(box->size, sizeof(*box->messages));
    if (!box->messages) { free(box); return ERR_MEM; }
    pthread_mutex_init(&box->lock, NULL);
    cond_init(&box->readable); cond_init(&box->writable);
    *out = box; return ERR_OK;
}
void sys_mbox_free(sys_mbox_t *box)
{
    pthread_cond_destroy(&(*box)->readable); pthread_cond_destroy(&(*box)->writable);
    pthread_mutex_destroy(&(*box)->lock);
    free((*box)->messages); free(*box); *box = NULL;
}
static void mbox_insert(struct un_sys_mbox *box, void *message)
{
    box->messages[(box->head + box->count) % box->size] = message;
    box->count++;
    pthread_cond_signal(&box->readable);
}
void sys_mbox_post(sys_mbox_t *box, void *message)
{
    pthread_mutex_lock(&(*box)->lock);
    while ((*box)->count == (*box)->size) pthread_cond_wait(&(*box)->writable, &(*box)->lock);
    mbox_insert(*box, message);
    pthread_mutex_unlock(&(*box)->lock);
}
err_t sys_mbox_trypost(sys_mbox_t *box, void *message)
{
    err_t result = ERR_MEM;
    pthread_mutex_lock(&(*box)->lock);
    if ((*box)->count < (*box)->size) { mbox_insert(*box, message); result = ERR_OK; }
    pthread_mutex_unlock(&(*box)->lock);
    return result;
}
err_t sys_mbox_trypost_fromisr(sys_mbox_t *box, void *message) { return sys_mbox_trypost(box, message); }
u32_t sys_arch_mbox_fetch(sys_mbox_t *box, void **message, u32_t timeout)
{
    u32_t start = sys_now();
    pthread_mutex_lock(&(*box)->lock);
    while (!(*box)->count) {
        u32_t elapsed = sys_now() - start;
        if (timeout && elapsed >= timeout) { pthread_mutex_unlock(&(*box)->lock); return SYS_ARCH_TIMEOUT; }
        if (timeout) cond_wait(&(*box)->readable, &(*box)->lock, timeout - elapsed);
        else pthread_cond_wait(&(*box)->readable, &(*box)->lock);
    }
    if (message) *message = (*box)->messages[(*box)->head];
    (*box)->head = ((*box)->head + 1) % (*box)->size;
    (*box)->count--;
    pthread_cond_signal(&(*box)->writable);
    pthread_mutex_unlock(&(*box)->lock);
    return sys_now() - start;
}
u32_t sys_arch_mbox_tryfetch(sys_mbox_t *box, void **message)
{
    pthread_mutex_lock(&(*box)->lock);
    if (!(*box)->count) { pthread_mutex_unlock(&(*box)->lock); return SYS_MBOX_EMPTY; }
    if (message) *message = (*box)->messages[(*box)->head];
    (*box)->head = ((*box)->head + 1) % (*box)->size;
    (*box)->count--;
    pthread_cond_signal(&(*box)->writable);
    pthread_mutex_unlock(&(*box)->lock);
    return 0;
}
struct thread_start { lwip_thread_fn function; void *argument; };
static void *thread_main(void *opaque)
{
    struct thread_start start = *(struct thread_start *)opaque;
    free(opaque); start.function(start.argument); return NULL;
}
sys_thread_t sys_thread_new(const char *name, lwip_thread_fn function, void *argument, int stacksize, int priority)
{
    pthread_t thread;
    pthread_attr_t attr;
    struct thread_start *start = malloc(sizeof(*start));
    (void)name; (void)stacksize; (void)priority;
    LWIP_ASSERT("thread allocation", start != NULL);
    start->function = function; start->argument = argument;
    /* The console's default stack is not documented; lwIP's call chains are deep. */
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 1024 * 1024);
    int result = pthread_create(&thread, &attr, thread_main, start);
    pthread_attr_destroy(&attr);
    LWIP_ASSERT("pthread_create", result == 0);
    pthread_detach(thread);
    return thread;
}
unsigned int usbn_port_rand(void)
{
    /* Each process has its own PRNG state; this is TCP simulation, not a secret source. */
    static uint32_t state;
    sys_prot_t lock = sys_arch_protect();
    if (!state) state = sys_now() ^ (uint32_t)getpid() ^ 0x9e3779b9U;
    state ^= state << 13; state ^= state >> 17; state ^= state << 5;
    uint32_t value = state;
    sys_arch_unprotect(lock); return value;
}
