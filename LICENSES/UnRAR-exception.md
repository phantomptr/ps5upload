# UnRAR linking exception (and required notice)

ps5upload is licensed under **GPL-3.0-or-later** (see `../LICENSE`). Its `.rar`
support is built on the **UnRAR** source code (bundled and compiled into the
desktop `ps5upload-engine` binary via the `unrar` / `unrar_sys` crates). UnRAR
is **only used to *extract* RAR archives** — ps5upload does not, and the UnRAR
code here may not, be used to compress RAR or build a RAR-compatible archiver.

UnRAR's own license is reproduced verbatim in `UnRAR-license.txt`. It is
compatible with permissive use but adds a field-of-use restriction that the GPL
would otherwise forbid combining with GPL code (GPLv3 §7). To resolve that, the
copyright holder grants the following additional permission. (All GPL-3.0 code
in the desktop engine is authored by the ps5upload project — there is no
third-party GPL code linked with UnRAR — so this grant can be made directly.)

## Additional permission under GNU GPL version 3 section 7

> As a special exception, the copyright holders of ps5upload give you
> permission to combine ps5upload with the UnRAR source code (and the
> `unrar` / `unrar_sys` Rust crates that bundle it), and to convey the
> resulting work. You may extend this exception to your version of
> ps5upload, but you are not obligated to do so. If you do not wish to do
> so, delete this exception statement from your version.

## Required UnRAR notice (UnRAR license, paragraph 2)

Reproduced as required, verbatim:

> UnRAR source code may be used in any software to handle RAR archives without
> limitations free of charge, but cannot be used to develop RAR (WinRAR)
> compatible archiver and to re-create RAR compression algorithm, which is
> proprietary. Distribution of modified UnRAR source code in separate form or
> as a part of other software is permitted, provided that full text of this
> paragraph, starting from "UnRAR source code" words, is included in license,
> or in documentation if license is not available, and in source code comments
> of resulting package.

## Scope notes

- RAR is **desktop-only**: the UnRAR C++ does not link against Android's
  wide-char libc/libc++, so the dependency is target-gated to non-Android
  targets. The Android build contains no UnRAR code and is unaffected by this
  exception.
- The on-PS5 payload (which contains GPL-3.0 code vendored from
  ps5-payload-dev) is a **separate binary** from the engine and does not link
  with UnRAR — it is mere aggregation, not a combined work.
