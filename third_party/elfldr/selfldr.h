/* Copyright (C) 2025 John Törnblom

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

#include <stdint.h>
#include <unistd.h>

/**
 * Read a SELF from the given socket.
 **/
int selfldr_read(int fd, uint8_t **self, size_t *self_size);


/**
 * Spawn a new process that executes the given SELF file.
 **/
pid_t selfldr_spawn(int stdio, char* const argv[],
		    uint8_t *self, size_t self_size);
