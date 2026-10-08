/* Copyright (C) 2024 John Törnblom

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 3, or (at your option) any
later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program; see the file COPYING. If not, see
<http://www.gnu.org/licenses/>.  */

#include "kernel.h"
#include "klog.h"
#include "patch.h"
#include "payload.h"
#include "rtld.h"
#include "rtld_dlfcn.h"
#include "rtld_payload.h"
#include "syscall.h"


/**
 * Dependencies provided by the ELF linker.
 **/
extern unsigned char __bss_start[] __attribute__((weak));
extern unsigned char __bss_end[] __attribute__((weak));


/**
 * Entry point to the main program.
 **/
extern int main(int argc, char* argv[], char *envp[]);


/**
 * Remember the args passed to _start and the cpu state.
 **/
static payload_args_t* payload_args = 0;
static void* jmpbuf[32];


/**
 * Initialize payload runtime.
 **/
static int
payload_init(payload_args_t *args) {
  int *__isthreaded = 0;
  int error = 0;

  if((error=__crt_syscall_init(args))) {
    return error;
  }
  if((error=__kernel_init(args))) {
    return error;
  }
  if((error=__klog_init())) {
    return error;
  }

  if(!KERNEL_DLSYM(0x2, __isthreaded)) {
    klog_puts("Unable to resolve the symbol '__isthreaded'");
    return -1;
  }
  *__isthreaded = 1;

  if((error=__patch_init())) {
    klog_puts("Unable to initialize patches");
    return error;
  }
  if((error=__rtld_init())) {
    klog_puts("Unable to initialize rtld");
    return error;
  }

  return 0;
}


/**
 * Run the payload.
 **/
static int
payload_run(void) {
  const char* __progname = 0;
  char** (*getargv)(void) = 0;
  int (*getargc)(void) = 0;
  rtld_lib_t* lib = 0;
  char** environ = 0;
  char** argv = 0;
  int argc = 0;
  int err = 0;

  if((KERNEL_DLSYM(0x1, getargc) || KERNEL_DLSYM(0x2001, getargc)) &&
     (KERNEL_DLSYM(0x1, getargv) || KERNEL_DLSYM(0x2001, getargv))) {
    argc = getargc();
    argv = getargv();
  }

  if(!(KERNEL_DLSYM(0x1, environ))) {
    if(!(KERNEL_DLSYM(0x2001, environ))) {
      environ = 0;
    }
  }

  if(!(KERNEL_DLSYM(0x1, __progname))) {
    if(!(KERNEL_DLSYM(0x2001, __progname))) {
      __progname = "";
    }
  }

  if(!(lib=__rtld_payload_new(__progname))) {
    return -1;
  }

  __rtld_dlfcn_setroot(lib);
  if((err=__rtld_lib_open(lib))) {
    __rtld_lib_destroy(lib);
    return err;
  }

  // run .init constructors
  if((err=__rtld_lib_init(lib, argc, argv, environ))) {
    __rtld_lib_close(lib);
    __rtld_lib_destroy(lib);
    return err;
  }

  // run the actual payload
  err = main(argc, argv, environ);
  if(payload_args->payloadout) {
    *payload_args->payloadout = err;
  }

  // run .fini destructors
  if((err=__rtld_lib_fini(lib))) {
    __rtld_lib_close(lib);
    __rtld_lib_destroy(lib);
    return err;
  }

  err = __rtld_lib_close(lib);
  __rtld_lib_destroy(lib);

  return err;
}


/**
 * Terminate the payload.
 **/
static int
payload_terminate(void) {
  void (*exit)(int) = 0;
  int exit_code = 0;

  // we are running inside a hijacked process, just return
  if(kernel_dynlib_dlsym(-1, 0x2001, "sceKernelDlsym")) {
    return exit_code;
  }

  if(payload_args->payloadout) {
    exit_code = *payload_args->payloadout;
  }

  // resolve and run exit
  if(KERNEL_DLSYM(0x2, exit)) {
    exit(exit_code);
  }

  // should not happend
  __builtin_trap();

  return -1;
}


/**
 * Exit the payload by transfering the flow of control back to _start().
 **/
void
payload_exit(int exit_code) {
  if(payload_args->payloadout) {
    *payload_args->payloadout = exit_code;
  }
  __builtin_longjmp(jmpbuf, 1);
}


/**
 * Provide a convenience function for accessing the payload args.
 **/
payload_args_t*
payload_get_args(void) {
  return payload_args;
}


/**
 * Entry-point invoked by the ELF loader.
 **/
int
__crt_start(payload_args_t *args) {
  int err;

  // clear .bss section
  for(unsigned char* bss=__bss_start; bss<__bss_end; bss++) {
    *bss = 0;
  }

  payload_args = args;

  // init payload runtime
  if((err=payload_init(args))) {
    if(args->payloadout) {
      *args->payloadout = err;
    }
    return payload_terminate();
  }

  // run payload
  if(!__builtin_setjmp(jmpbuf)) {
    if((err=payload_run())) {
      if(args->payloadout) {
	*args->payloadout = err;
      }
    }
  }

  // terminate payload
  return payload_terminate();
}
