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

/*
 * MC4 files decrypt to XML that was serialised as an escaped string: the real
 * plaintext of etaHEN's CUSA00002_01.00.mc4 begins
 *     &lt;?xml version=\&quot;1.0\&quot; encoding=\&quot;utf-16\&quot;?&gt;
 * i.e. every '<' '>' '"' is an XML entity and every '"' also carries a
 * backslash. The tag scan never sees "<Cheat" in that, so every real MC4 file
 * parsed to zero cheats even though decryption was correct (R16, #373).
 *
 * If the text (after leading whitespace/NULs) starts with "&lt;", decode it in
 * place: &lt; &gt; &quot; &amp; &apos; &#N; &#xN; and the backslash escapes
 * \" \\ \/ \r \n \t. Text that already starts with '<' (or anything else)
 * is left untouched. The result is never longer than the input; it is
 * NUL-terminated and the new length is returned (the old length if untouched).
 * `len` is the byte length; buf[len] need not be writable beyond it.
 */
size_t xml_unescape_entities(char *buf, size_t len);

#endif /* PS5UPLOAD_XML_ENCODING_H */
