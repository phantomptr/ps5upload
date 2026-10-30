# Third-party licenses

ps5upload itself is GPL-3.0-or-later (`../LICENSE`). Third-party code and assets keep their own
licenses. This folder holds the texts that do not live next to the code; the README's
[Credits](../README.md#credits) section lists every project and how it is used.

| File | Covers |
|---|---|
| `AGPL-3.0.txt` | The Remote Play streaming code ported from chiaki-ng (`engine/crates/ps5upload-core/src/rp_stream/`) |
| `playactor-ISC.txt` | The Remote Play registration and control-session code ported from playactor (`rp_regist.rs`, `rp_regist_keys.rs`, `rp_session.rs`) |
| `UnRAR-license.txt`, `UnRAR-exception.md` | The UnRAR source compiled into the separate `ps5upload-unrar` program for `.rar` extraction (the engine runs it over pipes, never links it), and the GPLv3 §7 permission to combine it |
| `Noto-OFL-1.1.txt` | The Noto Sans font subsets in `client/src/assets/fonts/` |
| `FidelityFX-FSR-MIT.txt` | The Remote Play upscaling shaders (EASU, RCAS) ported from AMD FidelityFX Super Resolution 1 in `client/src/lib/rpPicture.ts` |

License texts kept next to the code:

| Location | License |
|---|---|
| `payload/third_party/sdk-crt/LICENSE`, `third_party/elfldr/LICENSE` | GPL-3.0-or-later (John Törnblom) |
| `payload/third_party/libsmb2/COPYING`, `LICENCE-LGPL-2.1.txt` | LGPL-2.1-or-later |
| `payload/third_party/7zip/DOC/License.txt`, `copying.txt` | LGPL-2.1-or-later, parts BSD-3-Clause / BSD-2-Clause |
| `payload/third_party/libarchive/COPYING` | BSD-2-Clause |
| `payload/third_party/sqlite3/`, `payload/third_party/tiny_aes/` | Public domain / Unlicense (stated in the source) |
| `third_party/ava1/c/third_party/monocypher/LICENCE.md` | CC0-1.0 or BSD-2-Clause |
| `third_party/ava1/c/third_party/blake3/LICENSE_*` | CC0-1.0 or Apache-2.0 (with or without LLVM exception) |
| `third_party/sevenz-rust2/LICENSE` | Apache-2.0 |
| `third_party/unrar/LICENSE-MIT`, `LICENSE-APACHE` | MIT or Apache-2.0 (the Rust wrapper) |
| `third_party/ava1/LICENSE-MIT`, `LICENSE-APACHE` | MIT or Apache-2.0 |
| Source file headers | ps5-app-dumper, PS5 Dump Forge, ampr_emu and elf-arsenal ports (GPL-3.0-or-later / GPL-3.0); FreeBSD `newfs` portions (BSD-3-Clause) |
