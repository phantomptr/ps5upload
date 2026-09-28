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

#pragma once

#define INCASSET(name, file)			\
  __asm__(".section .rodata\n"			\
	  ".global " #name "\n"			\
	  ".global " #name "_end\n"		\
	  ".global " #name "_size\n"		\
	  ".align 16\n"				\
	  #name ":\n"				\
	  ".incbin \"" file "\"\n"		\
	  #name "_end:\n"			\
	  #name "_size:\n"			\
	  ".quad " #name "_end - " #name "\n"	\
	  ".previous\n");			\
  extern const unsigned char name[];		\
  extern const unsigned long name##_size;
