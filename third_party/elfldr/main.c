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

#include <ps5/kernel.h>

#include "asset.h"
#include "elfldr.h"
#include "log.h"
#include "notify.h"
#include "pt.h"

INCASSET(bootstrap_elf, "bootstrap.elf");


/**
 * We are running inside bdj.elf, attach to SceRedisServer and run bootstrap.elf.
 **/
int
main() {
  pid_t mypid = getpid();
  uint8_t qa_flags[16];
  uint8_t caps[16];
  uint64_t authid;
  intptr_t vnode;
  pid_t vpid;
  int ret;

  notify("Bootstrapping elfldr.elf...");
  LOG_PUTS("Bootstrapping elfldr.elf...");

  if(elfldr_sanity_check(bootstrap_elf, bootstrap_elf_size)) {
    LOG_PUTS("bootstrap.elf is corrupted");
    return -1;
  }

  // enable debugging with ptrace
  if(kernel_get_qaflags(qa_flags)) {
    LOG_PUTS("kernel_get_qa_flags failed");
    return -1;
  }
  if(!(qa_flags[1] & 0x03)) {
    qa_flags[1] |= 0x03;
    if(kernel_set_qaflags(qa_flags)) {
      LOG_PUTS("kernel_set_qa_flags failed");
      return -1;
    }
  }

  // backup my privileges
  if(!(vnode=kernel_get_proc_rootdir(mypid))) {
    LOG_PUTS("kernel_get_proc_rootdir failed");
    return -1;
  }
  if(kernel_get_ucred_caps(mypid, caps)) {
    LOG_PUTS("kernel_get_ucred_caps failed");
    return -1;
  }
  if(!(authid=kernel_get_ucred_authid(mypid))) {
    LOG_PUTS("kernel_get_ucred_authid failed");
    return -1;
  }

  // launch bootstrap.elf inside SceRedisServer
  if((vpid=elfldr_find_pid("SceRedisServer")) < 0) {
    LOG_PUTS("elfldr_find_pid failed");
    return -1;
  } else if(elfldr_raise_privileges(mypid)) {
    LOG_PUTS("Unable to raise privileges");
    ret = -1;
  } else if(pt_attach(vpid)) {
    LOG_PERROR("pt_attach");
    ret = -1;
  } else {
    ret = elfldr_exec(vpid, -1, bootstrap_elf);
  }

  // restore my privileges
  if(kernel_set_proc_jaildir(mypid, vnode)) {
    LOG_PUTS("kernel_set_proc_jaildir failed");
    ret = -1;
  }
  if(kernel_set_proc_rootdir(mypid, vnode)) {
    LOG_PUTS("kernel_set_proc_rootdir failed");
    ret = -1;
  }
  if(kernel_set_ucred_caps(mypid, caps)) {
    LOG_PUTS("kernel_set_ucred_caps failed");
    ret = -1;
  }
  if(kernel_set_ucred_authid(mypid, authid)) {
    LOG_PUTS("kernel_set_ucred_authid failed");
    ret = -1;
  }

  return ret;
}

