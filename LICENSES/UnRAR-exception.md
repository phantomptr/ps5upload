# UnRAR linking exception (and required notice)

ps5upload is licensed under **GPL-3.0-or-later** (see `../LICENSE`). Its `.rar`
support is built on the **UnRAR** source code, compiled (via the `unrar` /
`unrar_sys` crates) into one program of its own, **`ps5upload-unrar`**
(`engine/crates/ps5upload-unrar`). UnRAR is **only used to *extract* RAR
archives** — ps5upload does not, and the UnRAR code here may not, be used to
compress RAR or build a RAR-compatible archiver.

`ps5upload-unrar` contains **only code the ps5upload project holds the
copyright to, plus UnRAR** (and the permissively licensed Rust crates it needs:
`unrar`, `serde`, `serde_json`). It is a separate executable: the engine
(`ps5upload-engine`, which the desktop app and the Docker images also carry)
does **not** link UnRAR; it starts `ps5upload-unrar` and exchanges requests and
decoded bytes with it over pipes. The engine contains third-party GPL/AGPL code
(for example the Remote Play streaming port in `ps5upload-core/src/rp_stream`
and the AMPR emulation port in `ps5upload-fpkg`) whose authors grant no UnRAR
exception, which is why UnRAR must stay out of that binary. The desktop app and
the release/Docker packages ship `ps5upload-unrar` next to the engine.

UnRAR's own license is reproduced verbatim in `UnRAR-license.txt`. It is
compatible with permissive use but adds a field-of-use restriction that the GPL
would otherwise forbid combining with GPL code (GPLv3 §7). To resolve that, the
copyright holder grants the following additional permission. It covers the
code the ps5upload project holds the copyright to, which is all the GPL code in
`ps5upload-unrar`. (The PS5 Dump Forge image writers in
`engine/crates/ps5-dump-forge-*` and the fake-signing port in
`engine/crates/ps5upload-fpkg/src/fself.rs` also carry their authors' own,
identically worded exception, PS5 Dump Forge's
`vendor/LICENSE-UnRAR-exception.md`; they are not part of `ps5upload-unrar`.)

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

- **Keep `ps5upload-unrar` that way.** It must not depend on `ps5upload-core`
  or any other crate that carries third-party GPL/AGPL code, and no binary that
  does (the engine, the desktop app) may link the `unrar` crate. CI checks the
  engine and the desktop app with `cargo tree` (no `unrar` in either).
- RAR is **desktop-only**: there is no `ps5upload-unrar` for Android (the
  UnRAR C++ does not link against Android's wide-char libc/libc++), and the
  engine's RAR code is compiled out there. The Android build contains no UnRAR
  code and is unaffected by this exception. Without the helper (for example a
  standalone engine copied without it) RAR archives report "RAR support is not
  installed".
- The on-PS5 payload (which contains GPL-3.0 code vendored from
  ps5-payload-dev) is a **separate binary** from the engine and does not link
  with UnRAR — it is mere aggregation, not a combined work.
- The on-PS5 archive extractor (`payload/extractor`, embedded in the helper)
  links the payload SDK's GPL-3.0 startup code, so it must not contain UnRAR
  code either: it reads RAR with libarchive's independent, BSD-licensed RAR
  readers, and the 7-Zip sources vendored for it (`payload/third_party/7zip`)
  leave out 7-Zip's UnRAR-derived RAR decoders. Encrypted RAR archives are not
  supported there for that reason.
