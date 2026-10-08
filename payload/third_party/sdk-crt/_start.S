/* Copyright (C) 2026 John Törnblom

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

.intel_syntax noprefix

.data
.local __crt_reg_backup
.type  __crt_reg_backup, @object
__crt_reg_backup:
	.quad 0

.text
.extern __crt_start
.type   __crt_start, @function

.text
.global _start
.type   _start, @function
_start:
	mov [__crt_reg_backup + rip], rsp
	and rsp, -16
	call __crt_start
	mov rsp, [__crt_reg_backup + rip]
	ret
