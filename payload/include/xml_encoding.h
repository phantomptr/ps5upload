/*
 * xml_encoding.h — down-convert UTF-16 cheat XML to UTF-8 before parsing.
 *
 * SHN and MC4 trainer files are authored by Windows tools that sometimes save
 * UTF-16 (a real `ff fe` / `fe ff` BOM, `<?xml ... encoding="utf-16"?>`). The
 * cheat parser scans bytes for ASCII tags such as "<Cheat", which never match
 * UTF-16's `<\0C\0h\0e\0a\0t\0`, so those files silently parse to zero cheats
 * ("downloaded cheats don't appear for some games", issue #317). Converting
 * UTF-16 to UTF-8 first makes the existing ASCII-oriented parser work.
 */
#ifndef PS5UPLOAD_XML_ENCODING_H
#define PS5UPLOAD_XML_ENCODING_H

#include <stddef.h>

/*
 * If `in` is UTF-16 (LE or BE, with or without a BOM), return a freshly
 * malloc'd, NUL-terminated UTF-8 copy and set *out_len to its length (not
 * counting the terminator). The caller owns the buffer and must free() it.
 *
 * Returns NULL when the input is already 8-bit (UTF-8/ASCII) — no conversion
 * is needed, so the caller keeps parsing the original bytes — and also on
 * allocation failure, which degrades to the same "parse the original" path.
 * *out_len is set to 0 in both NULL cases.
 */
char *xml_to_utf8(const char *in, size_t in_len, size_t *out_len);

#endif /* PS5UPLOAD_XML_ENCODING_H */
