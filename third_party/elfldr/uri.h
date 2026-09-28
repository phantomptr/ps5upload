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

#include <unistd.h>


/**
 * Read the content from the given URI. Supported protocols are:
 * - file://
 * - http://
 * - https://
 **/
int uri_get_content(const char* uri, uint8_t** content, size_t* content_size);


/**
 * Get the value of a named parameter associated with the given URI.
 **/
char* uri_get_param(const char* uri, const char* name);


/**
 * Get the filename associated with the given URI.
 **/
char* uri_get_filename(const char* uri);
