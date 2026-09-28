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

#include <signal.h>
#include <stdio.h>

#include <ps5/kernel.h>

#include "asset.h"
#include "elfldr.h"
#include "log.h"
#include "notify.h"
#include "pt.h"

INCASSET(socksrv_elf, "socksrv.elf");


/**
 *
 **/
static char* const argv[] = {"elfldr.elf", 0};


/**
 * We are running inside SceRedisServer, spawn socksrv.elf.
 **/
int
main() {
  pid_t mypid = getpid();
  uint8_t caps[16];
  uint64_t authid;
  intptr_t vnode;
  int ret;

  notify("Spawning elfldr.elf...");
  LOG_PUTS("Spawning elfldr.elf...");

  if(elfldr_sanity_check(socksrv_elf, socksrv_elf_size)) {
    LOG_PUTS("socksrv.elf is corrupted");
    return -1;
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

  // launch socksrv.elf in a new processes
  if(elfldr_raise_privileges(mypid)) {
    LOG_PUTS("Unable to raise privileges");
    ret = -1;
  } else {
    signal(SIGCHLD, SIG_IGN);
    ret = elfldr_spawn(-1, argv, socksrv_elf, socksrv_elf_size);
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
