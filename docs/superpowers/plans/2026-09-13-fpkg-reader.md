# FPKG crate and reader (gate G0) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new `ps5upload-fpkg` crate that opens a debug PS5 FPKG, decrypts its outer PFS, and recomputes every digest we understand, passing on both local debug samples. This is the independent checker the writer (Plan 3) is verified with.

**Architecture:** Small focused modules: `crypto` (SHA3, HMAC, key derivation, AES-128-XTS, CRC-32C), `fih` and `cnt` (container headers and entries), `si` (the trailing STORED ZIP), `outer` (superblock, inodes, directory entries), and `verify` (a named list of checks). Reads go through a `PkgFile` that seeks, so memory stays bounded. The inner image (`pfs_image.dat` contents) is out of scope here: real samples compress it with Kraken, and the writer stores it raw, so Plan 3 verifies that layer by round trip.

**Tech Stack:** Rust 2021; `aes` 0.9, `hmac` 0.13, `sha2` 0.11 (already in the lockfile), `sha3` 0.11 (new), `thiserror` (workspace).

**Spec:** `docs/superpowers/specs/2026-09-13-fpkg-builder-design.md` — Part B, "Format facts" and gate G0.

## Global Constraints

- Write our own implementation. LibProsperoPKG and the other reference projects are read for understanding only; never copy or link their code.
- Every format constant in this plan was measured on `/Volumes/Storage/PS5/pkgs/webbrowser.pkg` and `EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg` on 2026-09-13. A test that disagrees with a real sample means the code is wrong, not the sample.
- Sample tests read `PS5UPLOAD_SAMPLE_PKGS` (default `/Volumes/Storage/PS5/pkgs`) and skip with an `eprintln!` when the file is absent, so CI without the drive stays green.
- Run `cargo fmt` after every Rust edit; `cargo clippy --workspace --all-targets -- -D warnings` must stay clean.
- After adding a dependency, commit `engine/Cargo.lock` and confirm `cargo check --workspace --locked`.
- Commits stage only the files the task names.

## Verified format facts this plan encodes

| Fact | Value |
|---|---|
| Block size | `0x10000` |
| FIH (little-endian) | magic `7F 46 49 48` at 0; signed byte at `0x05` (`0x00` debug); format u16 at `0x06` (= 3); PFS offset u64 `0x10`; PFS size u64 `0x18`; game digest `0x30..0x50`; CNT offset u64 `0x58` |
| CNT (big-endian, offsets relative to CNT start) | magic `7F 43 4E 54`; entry count u32 `0x10`; entry table offset u32 `0x18`; body offset u64 `0x20`; body size u64 `0x28`; content id `0x40..0x64`; digest-table digest `0x100..0x120`; package digest `0xFE0..0x1000` = `SHA3(CNT[0..0xFE0])` |
| CNT entry (0x20 bytes) | `id, name_off, flags1, flags2, data_off, data_size` as u32 BE |
| Entry `0x0001` | 32-byte `SHA3(payload)` per entry in table order; its own slot is zero; `CNT+0x100 = SHA3(entry 0x0001 payload)` |
| Entry `0x040A` (imagedigs) | one 32-byte digest per outer block = `SHA3(plaintext block)` **byte-reversed** |
| Keys | `EKPFS = SHA3(SHA3(BE32 1) ‖ SHA3(content id padded with NUL to 48) ‖ passcode ASCII)`; default passcode `"0"×32`; `K = HMAC-SHA256(key=EKPFS, msg=seed)`; `enc = HMAC-SHA256(key=K, msg=LE32(1) ‖ seed)`; tweak key `enc[0..16]`, data key `enc[16..32]` |
| XTS | AES-128-XTS, one 64 KiB block per data unit, tweak = sector as 16-byte LE. Data blocks: sector = block index. Signed blocks: sector = `(1<<47) | index`. The superblock block is plaintext. |
| Superblock | block whose `SHA3` equals the FIH game digest. Version u64 `0x00` = 2; magic u64 `0x08` = 20130315; block size u32 `0x20`; dinode count u64 `0x30`; ndblock u64 `0x38`; dinode block count u64 `0x40`; inode-table digest `0xB8..0xD8`; inode-table block u32 `0xD8`; seed `0x370..0x380`; ICV `0x380..0x3A0` = `SHA3(superblock[0..0x5A0] with the ICV zeroed)` |
| Dinode (0x2C8 bytes) | mode u16 `0x00`; nlink u16 `0x02`; flags u32 `0x04`; size u64 `0x08`; size_compressed u64 `0x10`; blocks u32 `0x60`; 12 direct `{sha3 32B, block u32}` at `0x64`; 5 indirect at `0x1F4`. Each direct signature = `SHA3(plaintext block)`. |
| Dirent | `ino u32, type i32, name_len u32, ent_size u32, name`; `ent_size = align8(name_len + 17)`; types file 2, dir 3, `.` 4, `..` 5 |
| Outer tree | inode 0 super-root → `inode_flat_path_table` (1) and `uroot` (2); `uroot` → `.`, `..`, `pfs_image.dat` (3), `naps_pkg_layout.dat` (4) |
| `playgo-chunk.crc` | little-endian CRC-32C (Castagnoli, reflected `0x82F63B78`) of each 64 KiB block of the file before the SI ZIP |

---

## File Structure

| File | Responsibility |
|---|---|
| `engine/Cargo.toml` | Add `crates/ps5upload-fpkg` to members. |
| `engine/crates/ps5upload-fpkg/Cargo.toml` | Crate manifest. |
| `engine/crates/ps5upload-fpkg/src/lib.rs` | Module list, `Error`, `PkgFile`, byte helpers. |
| `engine/crates/ps5upload-fpkg/src/crypto.rs` | SHA3, HMAC-SHA256, key derivation, CRC-32C. |
| `engine/crates/ps5upload-fpkg/src/xts.rs` | AES-128-XTS for one data unit. |
| `engine/crates/ps5upload-fpkg/src/fih.rs` | FIH header. |
| `engine/crates/ps5upload-fpkg/src/cnt.rs` | CNT header, entries, digest checks. |
| `engine/crates/ps5upload-fpkg/src/si.rs` | STORED ZIP member listing. |
| `engine/crates/ps5upload-fpkg/src/outer.rs` | Outer PFS: decrypt, superblock, dinodes, dirents. |
| `engine/crates/ps5upload-fpkg/src/verify.rs` | `verify_package` and `Report`. |
| `engine/crates/ps5upload-fpkg/tests/samples.rs` | Real-sample end-to-end checks. |
| `engine/crates/ps5upload-fpkg/examples/fpkg_verify.rs` | CLI: print the report for a path. |

---

### Task 1: Crate scaffold and primitives

**Files:**
- Modify: `engine/Cargo.toml`
- Create: `engine/crates/ps5upload-fpkg/Cargo.toml`, `src/lib.rs`, `src/crypto.rs`

**Interfaces:**
- Produces:
  - `pub const BLOCK: u64 = 0x10000;`
  - `pub enum Error` (thiserror) with `Io(#[from] std::io::Error)` and `Format(String)`; `pub type Result<T> = std::result::Result<T, Error>;`
  - `pub struct PkgFile` with `pub fn open(path: &Path) -> Result<Self>`, `pub fn len(&self) -> u64`, `pub fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>>`
  - `pub(crate) fn be32(b: &[u8], at: usize) -> u32`, `be64`, `le16`, `le32`, `le64`, `i32le`
  - `crypto::sha3(data: &[u8]) -> [u8; 32]`, `crypto::hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32]`, `crypto::derive_ekpfs(content_id: &str, passcode: &str) -> [u8; 32]`, `crypto::derive_xts_keys(ekpfs: &[u8; 32], seed: &[u8; 16]) -> XtsKeys { pub tweak: [u8; 16], pub data: [u8; 16] }`, `crypto::crc32c(data: &[u8]) -> u32`, `crypto::DEFAULT_PASSCODE: &str`

- [ ] **Step 1: Scaffold the crate**

Add `"crates/ps5upload-fpkg",` to `members` in `engine/Cargo.toml`.

`engine/crates/ps5upload-fpkg/Cargo.toml`:

```toml
[package]
name = "ps5upload-fpkg"
edition.workspace = true
license.workspace = true
version.workspace = true

[dependencies]
aes = "0.9"
hmac = "0.13"
sha2 = "0.11"
sha3 = "0.11"
thiserror.workspace = true
```

`src/lib.rs`:

```rust
//! Read (and later write) PS5 debug FPKG packages.
//!
//! Written from the format as measured on real packages; see
//! docs/superpowers/specs/2026-09-13-fpkg-builder-design.md.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub mod crypto;

/// Every PFS and finalized-image block is 64 KiB.
pub const BLOCK: u64 = 0x10000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn format_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}

/// A package on disk, read by offset.
pub struct PkgFile {
    file: std::fs::File,
    len: u64,
}

impl PkgFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Exactly `len` bytes at `off`, or an error if the file is shorter.
    pub fn read_at(&mut self, off: u64, len: usize) -> Result<Vec<u8>> {
        let end = off.checked_add(len as u64);
        if end.is_none_or(|e| e > self.len) {
            return format_err(format!("read of {len} bytes at {off:#x} past end {:#x}", self.len));
        }
        self.file.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }
}

pub(crate) fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
pub(crate) fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}
pub(crate) fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
pub(crate) fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
pub(crate) fn i32le(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
```

- [ ] **Step 2: Write the failing crypto tests**

`src/crypto.rs`, tests only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn crc32c_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// Values from Python's hashlib/hmac for the DLC sample's content id and
    /// its superblock seed; the same keys decrypt that real package.
    #[test]
    fn key_derivation_matches_real_package() {
        let ekpfs = derive_ekpfs("EP7579-PPSA17599_00-EXP33DLC10000PS5", DEFAULT_PASSCODE);
        assert_eq!(
            hex(&ekpfs),
            "6f2545336d611af09f15be54263809f1e2f0bc0062335e637463a9e7bee39b73"
        );
        let seed: [u8; 16] = [
            0x3a, 0xce, 0x52, 0x0f, 0x1d, 0xdb, 0xd3, 0xcb, 0x8a, 0xcb, 0xc4, 0x54, 0x3f, 0x95,
            0xfb, 0xa6,
        ];
        let keys = derive_xts_keys(&ekpfs, &seed);
        assert_eq!(hex(&keys.tweak), "04c9c2c17ea72b2be706be8ae56c9bff");
        assert_eq!(hex(&keys.data), "66cb5805c75c915a157f8ab9ccb80cab");
    }
}
```

Add `pub mod crypto;` is already in `lib.rs`.

- [ ] **Step 3: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg crypto`
Expected: compile errors for `crc32c`, `derive_ekpfs`, `derive_xts_keys`, `DEFAULT_PASSCODE`.

- [ ] **Step 4: Implement**

Put above the tests in `src/crypto.rs`:

```rust
//! Hashes, key derivation and CRC used by PS5 packages.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sha3::{Digest, Sha3_256};

/// The passcode debug FPKGs are built with when none is chosen.
pub const DEFAULT_PASSCODE: &str = "00000000000000000000000000000000";

pub fn sha3(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha3_256::digest(data));
    out
}

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    for p in parts {
        mac.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// The PFS image key for a debug package, from its content id and passcode.
pub fn derive_ekpfs(content_id: &str, passcode: &str) -> [u8; 32] {
    let mut cid = [0u8; 48];
    let id = content_id.as_bytes();
    cid[..id.len().min(48)].copy_from_slice(&id[..id.len().min(48)]);
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&sha3(&1u32.to_be_bytes()));
    buf.extend_from_slice(&sha3(&cid));
    buf.extend_from_slice(passcode.as_bytes());
    sha3(&buf)
}

pub struct XtsKeys {
    pub tweak: [u8; 16],
    pub data: [u8; 16],
}

/// AES-XTS key pair for an image with this superblock seed.
pub fn derive_xts_keys(ekpfs: &[u8; 32], seed: &[u8; 16]) -> XtsKeys {
    let k = hmac_sha256(ekpfs, &[seed]);
    let enc = hmac_sha256(&k, &[&1u32.to_le_bytes(), seed]);
    let mut tweak = [0u8; 16];
    let mut data = [0u8; 16];
    tweak.copy_from_slice(&enc[..16]);
    data.copy_from_slice(&enc[16..]);
    XtsKeys { tweak, data }
}

const CRC32C_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0x82F6_3B78 } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32C (Castagnoli), as stored in `playgo-chunk.crc`.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC32C_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}
```

- [ ] **Step 5: Run tests, check the lockfile**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg crypto && cargo tree -p ps5upload-fpkg -d`
Expected: 2 passed. `cargo tree -d` shows no second `digest` version; if `sha3 0.11` pulls a different `digest`, pick the `sha3` release that depends on `digest 0.11.3` and note it in the commit.

- [ ] **Step 6: Commit**

```bash
git add engine/Cargo.toml engine/Cargo.lock engine/crates/ps5upload-fpkg
git commit -m "feat(fpkg): crate scaffold with SHA3, HMAC, key derivation and CRC-32C"
```

---

### Task 2: AES-128-XTS

**Files:**
- Create: `engine/crates/ps5upload-fpkg/src/xts.rs`
- Modify: `src/lib.rs` (`pub mod xts;`)

**Interfaces:**
- Consumes: `crypto::XtsKeys`.
- Produces: `pub struct Xts` with `pub fn new(keys: &crypto::XtsKeys) -> Self`, `pub fn encrypt(&self, sector: u64, buf: &mut [u8])`, `pub fn decrypt(&self, sector: u64, buf: &mut [u8])`; `pub const SIGNED_SECTOR_FLAG: u64 = 1 << 47;`. `buf.len()` must be a non-zero multiple of 16 (every PFS block is).

- [ ] **Step 1: Failing tests (vectors from Python `cryptography`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{sha3, XtsKeys};

    fn keys() -> XtsKeys {
        let mut data = [0u8; 16];
        let mut tweak = [0u8; 16];
        for i in 0..16 {
            data[i] = i as u8;
            tweak[i] = 16 + i as u8;
        }
        XtsKeys { tweak, data }
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn two_blocks_sector_zero() {
        let mut buf = [0u8; 32];
        Xts::new(&keys()).encrypt(0, &mut buf);
        assert_eq!(
            hex(&buf),
            "f071a2b402c105ea37024133e24d6ef6212e8cc0175e1b6b32657d54f159daf6"
        );
    }

    #[test]
    fn full_block_data_and_signed_sectors() {
        let plain: Vec<u8> = (0..0x10000u32).map(|i| (i * 7) as u8).collect();
        let x = Xts::new(&keys());

        let mut data = plain.clone();
        x.encrypt(5, &mut data);
        assert_eq!(hex(&data[..16]), "7cd15324d71af9c318a1a7cd1cdf33e4");
        assert_eq!(
            hex(&sha3(&data)),
            "4b08520fde76f492d305b9bf740a294a0ccd64cbbd3e2783aebdd06c9353c483"
        );

        let mut signed = plain.clone();
        x.encrypt(SIGNED_SECTOR_FLAG | 5, &mut signed);
        assert_eq!(hex(&signed[..16]), "20e0ed830c5b7fa7536743e0f93b91fb");
        assert_eq!(
            hex(&sha3(&signed)),
            "e4b131dcb9d6fadb69de4ff7b0737f3f519ec6e57b5119c2a3148b53f2e54d42"
        );

        x.decrypt(5, &mut data);
        assert_eq!(data, plain);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg xts`
Expected: compile error, `Xts` / `SIGNED_SECTOR_FLAG` not found.

- [ ] **Step 3: Implement**

```rust
//! AES-128-XTS for one data unit (IEEE 1619), as PS5 PFS images use it.
//!
//! One 64 KiB block is one data unit; its sector number is the tweak.
//! Blocks are whole multiples of 16 bytes, so ciphertext stealing is never
//! needed.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::{Aes128, Block};

use crate::crypto::XtsKeys;

/// Set on the sector number of signed (metadata) blocks.
pub const SIGNED_SECTOR_FLAG: u64 = 1 << 47;

pub struct Xts {
    data: Aes128,
    tweak: Aes128,
}

/// Multiply the tweak by x in GF(2^128), little-endian byte order.
fn double(t: &mut [u8; 16]) {
    let carry = t[15] >> 7;
    for i in (1..16).rev() {
        t[i] = (t[i] << 1) | (t[i - 1] >> 7);
    }
    t[0] <<= 1;
    if carry != 0 {
        t[0] ^= 0x87;
    }
}

impl Xts {
    pub fn new(keys: &XtsKeys) -> Self {
        Self {
            data: Aes128::new(&keys.data.into()),
            tweak: Aes128::new(&keys.tweak.into()),
        }
    }

    fn start_tweak(&self, sector: u64) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..8].copy_from_slice(&sector.to_le_bytes());
        let mut blk = Block::from(t);
        self.tweak.encrypt_block(&mut blk);
        t.copy_from_slice(&blk);
        t
    }

    pub fn encrypt(&self, sector: u64, buf: &mut [u8]) {
        self.apply(sector, buf, true);
    }

    pub fn decrypt(&self, sector: u64, buf: &mut [u8]) {
        self.apply(sector, buf, false);
    }

    fn apply(&self, sector: u64, buf: &mut [u8], encrypt: bool) {
        assert!(
            !buf.is_empty() && buf.len() % 16 == 0,
            "XTS data unit must be a non-zero multiple of 16 bytes"
        );
        let mut t = self.start_tweak(sector);
        for chunk in buf.chunks_exact_mut(16) {
            let mut x = [0u8; 16];
            for i in 0..16 {
                x[i] = chunk[i] ^ t[i];
            }
            let mut blk = Block::from(x);
            if encrypt {
                self.data.encrypt_block(&mut blk);
            } else {
                self.data.decrypt_block(&mut blk);
            }
            for i in 0..16 {
                chunk[i] = blk[i] ^ t[i];
            }
            double(&mut t);
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg xts`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src/xts.rs engine/crates/ps5upload-fpkg/src/lib.rs
git commit -m "feat(fpkg): AES-128-XTS for PFS data units"
```

---

### Task 3: FIH header and CNT container

**Files:**
- Create: `src/fih.rs`, `src/cnt.rs`
- Modify: `src/lib.rs` (`pub mod fih; pub mod cnt;`)

**Interfaces:**
- Consumes: `PkgFile`, `be32`, `be64`, `le16`, `le64`, `crypto::sha3`, `format_err`.
- Produces:
  - `fih::Fih { pub signed_byte: u8, pub format_version: u16, pub pfs_offset: u64, pub pfs_size: u64, pub game_digest: [u8; 32], pub cnt_offset: u64 }`, `pub const HEADER_LEN: usize = 0x100`, `pub fn parse(head: &[u8]) -> Result<Fih>`, `impl Fih { pub fn is_debug(&self) -> bool }`
  - `cnt::Entry { pub id: u32, pub name_off: u32, pub flags1: u32, pub flags2: u32, pub offset: u32, pub size: u32 }`
  - `cnt::Cnt { pub bytes: Vec<u8>, pub content_id: String, pub body_offset: u64, pub body_size: u64, pub entries: Vec<Entry> }`
  - `pub fn read(file: &mut PkgFile, cnt_offset: u64) -> Result<Cnt>` (reads through the end of the last entry)
  - `impl Cnt { pub fn entry(&self, id: u32) -> Option<&Entry>; pub fn payload(&self, e: &Entry) -> &[u8]; pub fn package_digest_ok(&self) -> bool; pub fn entry_digests(&self) -> Vec<(u32, EntryDigest)>; pub fn digest_table_digest_ok(&self) -> bool; pub fn image_digests(&self) -> Option<Vec<[u8; 32]>> }`
  - `pub enum EntryDigest { SelfSlotZero, Match, Mismatch }`
  - `pub mod ids { pub const DIGESTS: u32 = 0x0001; pub const PARAM_JSON: u32 = 0x2000; pub const IMAGE_DIGESTS: u32 = 0x040A; }`

- [ ] **Step 1: Failing tests (synthetic + real samples)**

In `src/cnt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::sha3;

    /// A minimal CNT: two entries (the digest table and one payload) with
    /// every digest filled in the way real packages fill them.
    fn synthetic() -> Vec<u8> {
        let mut c = vec![0u8; 0x3000];
        c[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        c[0x10..0x14].copy_from_slice(&2u32.to_be_bytes());
        c[0x18..0x1C].copy_from_slice(&0x2000u32.to_be_bytes());
        c[0x40..0x64].copy_from_slice(b"UP0000-PPSA01234_00-TESTGAME00000000");
        let table = 0x2000usize;
        let digests_off = 0x2100u32;
        let payload_off = 0x2200u32;
        let payload = b"{\"titleId\":\"PPSA01234\"}";
        let put = |c: &mut Vec<u8>, i: usize, id: u32, off: u32, size: u32| {
            let o = table + i * 0x20;
            c[o..o + 4].copy_from_slice(&id.to_be_bytes());
            c[o + 16..o + 20].copy_from_slice(&off.to_be_bytes());
            c[o + 20..o + 24].copy_from_slice(&size.to_be_bytes());
        };
        put(&mut c, 0, ids::DIGESTS, digests_off, 64);
        put(&mut c, 1, ids::PARAM_JSON, payload_off, payload.len() as u32);
        c[payload_off as usize..payload_off as usize + payload.len()].copy_from_slice(payload);
        let d = sha3(payload);
        c[digests_off as usize + 32..digests_off as usize + 64].copy_from_slice(&d);
        let table_digest = sha3(&c[digests_off as usize..digests_off as usize + 64]);
        c[0x100..0x120].copy_from_slice(&table_digest);
        let pkg = sha3(&c[..0xFE0]);
        c[0xFE0..0x1000].copy_from_slice(&pkg);
        c
    }

    #[test]
    fn synthetic_digests_verify() {
        let cnt = Cnt::from_bytes(synthetic()).unwrap();
        assert_eq!(cnt.content_id, "UP0000-PPSA01234_00-TESTGAME00000000");
        assert!(cnt.package_digest_ok());
        assert!(cnt.digest_table_digest_ok());
        assert_eq!(
            cnt.entry_digests(),
            vec![
                (ids::DIGESTS, EntryDigest::SelfSlotZero),
                (ids::PARAM_JSON, EntryDigest::Match)
            ]
        );
    }

    #[test]
    fn tampered_payload_is_caught() {
        let mut bytes = synthetic();
        bytes[0x2200] ^= 1;
        let cnt = Cnt::from_bytes(bytes).unwrap();
        assert_eq!(cnt.entry_digests()[1], (ids::PARAM_JSON, EntryDigest::Mismatch));
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = synthetic();
        bytes[0] = 0;
        assert!(Cnt::from_bytes(bytes).is_err());
    }
}
```

In `src/fih.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_debug_header() {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&0x7F46_4948u32.to_be_bytes());
        h[5] = 0x00;
        h[6..8].copy_from_slice(&3u16.to_le_bytes());
        h[0x10..0x18].copy_from_slice(&0x10000u64.to_le_bytes());
        h[0x18..0x20].copy_from_slice(&0x70000u64.to_le_bytes());
        h[0x30] = 0xAB;
        h[0x58..0x60].copy_from_slice(&0x80000u64.to_le_bytes());
        let f = parse(&h).unwrap();
        assert!(f.is_debug());
        assert_eq!((f.pfs_offset, f.pfs_size, f.cnt_offset), (0x10000, 0x70000, 0x80000));
        assert_eq!(f.game_digest[0], 0xAB);
    }

    #[test]
    fn rejects_cnt_magic() {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        assert!(parse(&h).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg -- fih cnt`
Expected: compile errors (`parse`, `Cnt`, `ids`, … missing).

- [ ] **Step 3: Implement `fih.rs`**

```rust
//! The finalized-image (`\x7FFIH`) header. Little-endian.

use crate::{format_err, le16, le64, Result};

pub const HEADER_LEN: usize = 0x100;
const MAGIC: u32 = 0x7F46_4948;

pub struct Fih {
    pub signed_byte: u8,
    pub format_version: u16,
    pub pfs_offset: u64,
    pub pfs_size: u64,
    /// SHA3-256 of the plaintext outer superblock block.
    pub game_digest: [u8; 32],
    pub cnt_offset: u64,
}

impl Fih {
    /// `0x00` marks a debug image; retail images carry `0x80`.
    pub fn is_debug(&self) -> bool {
        self.signed_byte == 0
    }
}

pub fn parse(head: &[u8]) -> Result<Fih> {
    if head.len() < HEADER_LEN {
        return format_err("FIH header truncated");
    }
    if u32::from_be_bytes(head[0..4].try_into().unwrap()) != MAGIC {
        return format_err("not a finalized PS5 package (no \\x7FFIH magic)");
    }
    let mut game_digest = [0u8; 32];
    game_digest.copy_from_slice(&head[0x30..0x50]);
    Ok(Fih {
        signed_byte: head[5],
        format_version: le16(head, 6),
        pfs_offset: le64(head, 0x10),
        pfs_size: le64(head, 0x18),
        game_digest,
        cnt_offset: le64(head, 0x58),
    })
}
```

- [ ] **Step 4: Implement `cnt.rs`**

```rust
//! The `\x7FCNT` metadata container embedded in a finalized image.
//! Big-endian; every offset is relative to the start of the container.

use crate::crypto::sha3;
use crate::{be32, be64, format_err, PkgFile, Result};

const MAGIC: u32 = 0x7F43_4E54;
const ENTRY_LEN: usize = 0x20;
const HEADER_REGION: usize = 0x1000;

pub mod ids {
    pub const DIGESTS: u32 = 0x0001;
    pub const IMAGE_DIGESTS: u32 = 0x040A;
    pub const PARAM_JSON: u32 = 0x2000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub name_off: u32,
    pub flags1: u32,
    pub flags2: u32,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDigest {
    /// The digest table's own slot, which real packages leave zero.
    SelfSlotZero,
    Match,
    Mismatch,
}

pub struct Cnt {
    pub bytes: Vec<u8>,
    pub content_id: String,
    pub body_offset: u64,
    pub body_size: u64,
    pub entries: Vec<Entry>,
}

/// Read the container at `cnt_offset` through the end of its last entry.
pub fn read(file: &mut PkgFile, cnt_offset: u64) -> Result<Cnt> {
    let head = file.read_at(cnt_offset, HEADER_REGION)?;
    if be32(&head, 0) != MAGIC {
        return format_err("embedded CNT magic mismatch");
    }
    let count = be32(&head, 0x10) as usize;
    let table = be32(&head, 0x18) as usize;
    let table_bytes = file.read_at(cnt_offset + table as u64, count * ENTRY_LEN)?;
    let mut end = table + count * ENTRY_LEN;
    for i in 0..count {
        let o = i * ENTRY_LEN;
        let off = be32(&table_bytes, o + 16) as usize;
        let size = be32(&table_bytes, o + 20) as usize;
        end = end.max(off + size);
    }
    Cnt::from_bytes(file.read_at(cnt_offset, end.max(HEADER_REGION))?)
}

impl Cnt {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Cnt> {
        if bytes.len() < HEADER_REGION || be32(&bytes, 0) != MAGIC {
            return format_err("not a CNT container");
        }
        let count = be32(&bytes, 0x10) as usize;
        let table = be32(&bytes, 0x18) as usize;
        if table + count * ENTRY_LEN > bytes.len() {
            return format_err("CNT entry table out of range");
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let o = table + i * ENTRY_LEN;
            let e = Entry {
                id: be32(&bytes, o),
                name_off: be32(&bytes, o + 4),
                flags1: be32(&bytes, o + 8),
                flags2: be32(&bytes, o + 12),
                offset: be32(&bytes, o + 16),
                size: be32(&bytes, o + 20),
            };
            if e.offset as usize + e.size as usize > bytes.len() {
                return format_err(format!("CNT entry {:#06x} out of range", e.id));
            }
            entries.push(e);
        }
        let content_id = String::from_utf8_lossy(&bytes[0x40..0x64])
            .trim_end_matches('\0')
            .to_string();
        Ok(Cnt {
            content_id,
            body_offset: be64(&bytes, 0x20),
            body_size: be64(&bytes, 0x28),
            entries,
            bytes,
        })
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn payload(&self, e: &Entry) -> &[u8] {
        &self.bytes[e.offset as usize..e.offset as usize + e.size as usize]
    }

    /// `CNT+0xFE0 == SHA3(CNT[0..0xFE0])`.
    pub fn package_digest_ok(&self) -> bool {
        sha3(&self.bytes[..0xFE0]) == self.bytes[0xFE0..0x1000]
    }

    /// `CNT+0x100 == SHA3(entry 0x0001 payload)`.
    pub fn digest_table_digest_ok(&self) -> bool {
        match self.entry(ids::DIGESTS) {
            Some(e) => sha3(self.payload(e)) == self.bytes[0x100..0x120],
            None => false,
        }
    }

    /// Each entry's payload against its slot in the digest table.
    pub fn entry_digests(&self) -> Vec<(u32, EntryDigest)> {
        let Some(table) = self.entry(ids::DIGESTS).map(|e| self.payload(e)) else {
            return Vec::new();
        };
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let slot = table.get(i * 32..(i + 1) * 32).unwrap_or(&[]);
                let verdict = if e.id == ids::DIGESTS {
                    if slot.iter().all(|&b| b == 0) {
                        EntryDigest::SelfSlotZero
                    } else {
                        EntryDigest::Mismatch
                    }
                } else if slot == sha3(self.payload(e)) {
                    EntryDigest::Match
                } else {
                    EntryDigest::Mismatch
                };
                (e.id, verdict)
            })
            .collect()
    }

    /// `imagedigs.dat`: one digest per outer block, stored byte-reversed.
    /// Returned in natural order, i.e. directly comparable to `sha3(block)`.
    pub fn image_digests(&self) -> Option<Vec<[u8; 32]>> {
        let p = self.payload(self.entry(ids::IMAGE_DIGESTS)?);
        Some(
            p.chunks_exact(32)
                .map(|c| {
                    let mut d = [0u8; 32];
                    d.copy_from_slice(c);
                    d.reverse();
                    d
                })
                .collect(),
        )
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg -- fih cnt`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src
git commit -m "feat(fpkg): parse the FIH header and verify CNT digests"
```

---

### Task 4: SI ZIP and `playgo-chunk.crc`

**Files:**
- Create: `src/si.rs`
- Modify: `src/lib.rs` (`pub mod si;`)

**Interfaces:**
- Consumes: `PkgFile`, `le16`, `le32`, `crypto::crc32c`, `BLOCK`.
- Produces:
  - `si::Member { pub name: String, pub offset: u64, pub size: u64 }`
  - `si::Si { pub zip_start: u64, pub members: Vec<Member> }`
  - `pub fn read(file: &mut PkgFile) -> Result<Option<Si>>` — `None` when the package has no trailing STORED ZIP
  - `pub fn chunk_crc_table(file: &mut PkgFile, zip_start: u64) -> Result<Vec<u8>>` — the table a correct `playgo-chunk.crc` must equal

- [ ] **Step 1: Failing test (real sample only)**

`tests/samples.rs` is created in Task 6; for this task add a unit test that needs no fixture beyond the sample:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> Option<PkgFile> {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let p = std::path::Path::new(&dir).join(name);
        match PkgFile::open(&p) {
            Ok(f) => Some(f),
            Err(_) => {
                eprintln!("skip: {} not present", p.display());
                None
            }
        }
    }

    #[test]
    fn dlc_sample_si_members_and_crc() {
        let Some(mut f) = sample("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg") else {
            return;
        };
        let si = read(&mut f).unwrap().expect("debug package has an SI ZIP");
        assert_eq!(si.zip_start, 0x130000);
        let names: Vec<&str> = si.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "common/etc/naps_meta_18.dat",
                "common/etc/naps_meta_300.dat",
                "common/etc/naps_meta_301.dat",
                "common/etc/naps_meta_302.dat",
                "common/etc/naps_meta_308.dat",
                "common/etc/pfsimage.xml",
                "common/etc/playgo-chunk.dat",
                "config/EP7579-PPSA17599_00-EXP33DLC10000PS5/playgo-chunk.crc",
            ]
        );
        let crc = si.members.last().unwrap();
        assert_eq!((crc.offset, crc.size), (0x1324d8, 76));
        let stored = f.read_at(crc.offset, crc.size as usize).unwrap();
        assert_eq!(chunk_crc_table(&mut f, si.zip_start).unwrap(), stored);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg si`
Expected: compile errors (`read`, `chunk_crc_table`).

- [ ] **Step 3: Implement**

```rust
//! The SI segment: a STORED ZIP appended after the embedded CNT.

use crate::crypto::crc32c;
use crate::{format_err, le16, le32, PkgFile, Result, BLOCK};

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;

pub struct Member {
    pub name: String,
    /// Absolute offset of the member's bytes in the package.
    pub offset: u64,
    pub size: u64,
}

pub struct Si {
    /// Absolute offset where the ZIP (and the CRC-excluded tail) begins.
    pub zip_start: u64,
    pub members: Vec<Member>,
}

pub fn read(file: &mut PkgFile) -> Result<Option<Si>> {
    let total = file.len();
    if total < EOCD_LEN as u64 {
        return Ok(None);
    }
    let tail_len = total.min(EOCD_LEN as u64 + 0xFFFF);
    let tail_start = total - tail_len;
    let tail = file.read_at(tail_start, tail_len as usize)?;
    let Some(rel) = (0..=tail.len() - EOCD_LEN)
        .rev()
        .find(|&i| le32(&tail, i) == EOCD_SIG)
    else {
        return Ok(None);
    };
    let entries = le16(&tail, rel + 10) as usize;
    let cd_size = le32(&tail, rel + 12) as u64;
    let cd_off = le32(&tail, rel + 16) as u64;
    let eocd = tail_start + rel as u64;
    if cd_size == 0 || cd_size + cd_off > eocd {
        return Ok(None);
    }
    let cd_abs = eocd - cd_size;
    let zip_start = cd_abs - cd_off;
    let cd = file.read_at(cd_abs, cd_size as usize)?;

    let mut members = Vec::with_capacity(entries);
    let mut pos = 0usize;
    for _ in 0..entries {
        if pos + 46 > cd.len() || le32(&cd, pos) != CENTRAL_SIG {
            return format_err("SI central directory is malformed");
        }
        let method = le16(&cd, pos + 10);
        let size = le32(&cd, pos + 24) as u64;
        let name_len = le16(&cd, pos + 28) as usize;
        let extra = le16(&cd, pos + 30) as usize;
        let comment = le16(&cd, pos + 32) as usize;
        let local = zip_start + le32(&cd, pos + 42) as u64;
        let name = String::from_utf8_lossy(&cd[pos + 46..pos + 46 + name_len]).into_owned();
        pos += 46 + name_len + extra + comment;
        if method != 0 {
            return format_err(format!("SI member {name} is not STORED"));
        }
        let lh = file.read_at(local, 30)?;
        if le32(&lh, 0) != LOCAL_SIG {
            return format_err(format!("SI member {name} has no local header"));
        }
        let offset = local + 30 + le16(&lh, 26) as u64 + le16(&lh, 28) as u64;
        members.push(Member { name, offset, size });
    }
    Ok(Some(Si { zip_start, members }))
}

/// CRC-32C of every 64 KiB block before the SI ZIP, little-endian.
pub fn chunk_crc_table(file: &mut PkgFile, zip_start: u64) -> Result<Vec<u8>> {
    let blocks = zip_start.div_ceil(BLOCK);
    let mut out = Vec::with_capacity(blocks as usize * 4);
    for i in 0..blocks {
        let start = i * BLOCK;
        let len = (zip_start - start).min(BLOCK) as usize;
        out.extend_from_slice(&crc32c(&file.read_at(start, len)?).to_le_bytes());
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg si -- --nocapture`
Expected: 1 passed (no `skip:` line on this machine).

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src
git commit -m "feat(fpkg): read the SI ZIP and recompute playgo-chunk.crc"
```

---

### Task 5: Outer PFS

**Files:**
- Create: `src/outer.rs`
- Modify: `src/lib.rs` (`pub mod outer;`)

**Interfaces:**
- Consumes: `PkgFile`, `fih::Fih`, `cnt::Cnt`, `crypto::{sha3, derive_ekpfs, derive_xts_keys}`, `xts::{Xts, SIGNED_SECTOR_FLAG}`, `le16`, `le32`, `le64`, `i32le`, `BLOCK`.
- Produces:
  - `pub enum BlockKind { Data, Signed, Superblock }`
  - `pub struct BlockVerdict { pub index: u64, pub kind: Option<BlockKind> }` — `kind: None` means no decryption matched its image digest
  - `pub struct Superblock { pub index: u64, pub dinode_count: u64, pub ndblock: u64, pub inode_table_block: u32, pub inode_table_digest: [u8; 32], pub seed: [u8; 16], pub icv_ok: bool }`
  - `pub struct DirectBlock { pub digest: [u8; 32], pub block: u32 }`
  - `pub struct Dinode { pub mode: u16, pub nlink: u16, pub flags: u32, pub size: u64, pub size_compressed: u64, pub blocks: u32, pub direct: [DirectBlock; 12], pub indirect: [DirectBlock; 5] }`
  - `pub struct Dirent { pub ino: u32, pub kind: i32, pub name: String }`
  - `pub struct OuterImage { pub superblock: Superblock, pub plaintext: Vec<Vec<u8>>, pub verdicts: Vec<BlockVerdict> }`
  - `pub fn open(file: &mut PkgFile, fih: &Fih, cnt: &Cnt, passcode: &str) -> Result<OuterImage>`
  - `impl OuterImage { pub fn dinodes(&self) -> Vec<Dinode>; pub fn dirents(&self, dir: &Dinode) -> Vec<Dirent> }`
  - `pub const DINODE_LEN: usize = 0x2C8;`

`open` loads every outer block into memory. That is fine for the reader's job (verifying small packages and our own output in tests); a streaming verifier for multi-GB output belongs to Plan 3.

- [ ] **Step 1: Failing test on the DLC sample**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cnt, crypto::DEFAULT_PASSCODE, fih};

    #[test]
    fn dlc_sample_outer_tree() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg");
        let Ok(mut f) = PkgFile::open(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let head = f.read_at(0, fih::HEADER_LEN).unwrap();
        let fih = fih::parse(&head).unwrap();
        let cnt = cnt::read(&mut f, fih.cnt_offset).unwrap();
        let img = open(&mut f, &fih, &cnt, DEFAULT_PASSCODE).unwrap();

        assert_eq!(img.superblock.index, 2);
        assert!(img.superblock.icv_ok);
        assert_eq!(img.superblock.dinode_count, 5);
        let kinds: Vec<_> = img.verdicts.iter().map(|v| v.kind).collect();
        assert_eq!(
            kinds,
            [
                Some(BlockKind::Data),
                Some(BlockKind::Signed),
                Some(BlockKind::Superblock),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
                Some(BlockKind::Signed),
            ]
        );
        assert_eq!(
            sha3(&img.plaintext[img.superblock.inode_table_block as usize]),
            img.superblock.inode_table_digest
        );

        let nodes = img.dinodes();
        assert_eq!(nodes.len(), 5);
        for n in &nodes {
            for d in n.direct.iter().take(n.blocks as usize) {
                assert_eq!(sha3(&img.plaintext[d.block as usize]), d.digest);
            }
        }
        let names = |ino: usize| -> Vec<String> {
            img.dirents(&nodes[ino]).into_iter().map(|d| d.name).collect()
        };
        assert_eq!(names(0), ["inode_flat_path_table", "uroot"]);
        assert_eq!(names(2), [".", "..", "pfs_image.dat", "naps_pkg_layout.dat"]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg outer`
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
//! The outer PFS of a finalized image: decrypt, superblock, inodes, dirents.

use crate::cnt::Cnt;
use crate::crypto::{derive_ekpfs, derive_xts_keys, sha3};
use crate::fih::Fih;
use crate::xts::{Xts, SIGNED_SECTOR_FLAG};
use crate::{format_err, i32le, le16, le32, le64, PkgFile, Result, BLOCK};

pub const DINODE_LEN: usize = 0x2C8;
const SUPERBLOCK_MAGIC: u64 = 20_130_315;
const ICV: std::ops::Range<usize> = 0x380..0x3A0;
const SIGNED_REGION: usize = 0x5A0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Data,
    Signed,
    Superblock,
}

pub struct BlockVerdict {
    pub index: u64,
    pub kind: Option<BlockKind>,
}

pub struct Superblock {
    pub index: u64,
    pub dinode_count: u64,
    pub ndblock: u64,
    pub inode_table_block: u32,
    pub inode_table_digest: [u8; 32],
    pub seed: [u8; 16],
    pub icv_ok: bool,
}

#[derive(Clone, Copy)]
pub struct DirectBlock {
    pub digest: [u8; 32],
    pub block: u32,
}

pub struct Dinode {
    pub mode: u16,
    pub nlink: u16,
    pub flags: u32,
    pub size: u64,
    pub size_compressed: u64,
    pub blocks: u32,
    pub direct: [DirectBlock; 12],
    pub indirect: [DirectBlock; 5],
}

pub struct Dirent {
    pub ino: u32,
    pub kind: i32,
    pub name: String,
}

pub struct OuterImage {
    pub superblock: Superblock,
    pub plaintext: Vec<Vec<u8>>,
    pub verdicts: Vec<BlockVerdict>,
}

fn block_sig(b: &[u8], at: usize) -> DirectBlock {
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&b[at..at + 32]);
    DirectBlock {
        digest,
        block: le32(b, at + 32),
    }
}

fn parse_superblock(index: u64, sb: &[u8]) -> Result<Superblock> {
    if le64(sb, 0) != 2 || le64(sb, 8) != SUPERBLOCK_MAGIC {
        return format_err("outer superblock version/magic mismatch");
    }
    let mut zeroed = sb[..SIGNED_REGION].to_vec();
    zeroed[ICV].fill(0);
    let mut seed = [0u8; 16];
    seed.copy_from_slice(&sb[0x370..0x380]);
    let table = block_sig(sb, 0xB8);
    Ok(Superblock {
        index,
        dinode_count: le64(sb, 0x30),
        ndblock: le64(sb, 0x38),
        inode_table_block: table.block,
        inode_table_digest: table.digest,
        seed,
        icv_ok: sha3(&zeroed) == sb[ICV],
    })
}

pub fn open(file: &mut PkgFile, fih: &Fih, cnt: &Cnt, passcode: &str) -> Result<OuterImage> {
    if fih.pfs_size % BLOCK != 0 {
        return format_err("outer image size is not a whole number of blocks");
    }
    let Some(digests) = cnt.image_digests() else {
        return format_err("CNT has no imagedigs entry");
    };
    let count = fih.pfs_size / BLOCK;
    if digests.len() as u64 != count {
        return format_err("imagedigs length does not match the outer image");
    }
    let mut raw = Vec::with_capacity(count as usize);
    for i in 0..count {
        raw.push(file.read_at(fih.pfs_offset + i * BLOCK, BLOCK as usize)?);
    }
    let Some(sb_index) = raw.iter().position(|b| sha3(b) == fih.game_digest) else {
        return format_err("no outer block matches the FIH game digest");
    };
    let superblock = parse_superblock(sb_index as u64, &raw[sb_index])?;
    let ekpfs = derive_ekpfs(&cnt.content_id, passcode);
    let xts = Xts::new(&derive_xts_keys(&ekpfs, &superblock.seed));

    let mut plaintext = Vec::with_capacity(raw.len());
    let mut verdicts = Vec::with_capacity(raw.len());
    for (i, block) in raw.into_iter().enumerate() {
        let index = i as u64;
        if i == sb_index {
            plaintext.push(block);
            verdicts.push(BlockVerdict {
                index,
                kind: Some(BlockKind::Superblock),
            });
            continue;
        }
        let mut found = None;
        for (kind, sector) in [
            (BlockKind::Data, index),
            (BlockKind::Signed, SIGNED_SECTOR_FLAG | index),
        ] {
            let mut pt = block.clone();
            xts.decrypt(sector, &mut pt);
            if sha3(&pt) == digests[i] {
                found = Some((kind, pt));
                break;
            }
        }
        match found {
            Some((kind, pt)) => {
                plaintext.push(pt);
                verdicts.push(BlockVerdict {
                    index,
                    kind: Some(kind),
                });
            }
            None => {
                plaintext.push(block);
                verdicts.push(BlockVerdict { index, kind: None });
            }
        }
    }
    Ok(OuterImage {
        superblock,
        plaintext,
        verdicts,
    })
}

impl OuterImage {
    pub fn dinodes(&self) -> Vec<Dinode> {
        let Some(table) = self
            .plaintext
            .get(self.superblock.inode_table_block as usize)
        else {
            return Vec::new();
        };
        (0..self.superblock.dinode_count as usize)
            .take_while(|j| (j + 1) * DINODE_LEN <= table.len())
            .map(|j| {
                let o = j * DINODE_LEN;
                Dinode {
                    mode: le16(table, o),
                    nlink: le16(table, o + 2),
                    flags: le32(table, o + 4),
                    size: le64(table, o + 8),
                    size_compressed: le64(table, o + 0x10),
                    blocks: le32(table, o + 0x60),
                    direct: std::array::from_fn(|k| block_sig(table, o + 0x64 + k * 36)),
                    indirect: std::array::from_fn(|k| block_sig(table, o + 0x1F4 + k * 36)),
                }
            })
            .collect()
    }

    /// Directory entries of a directory inode's first block.
    pub fn dirents(&self, dir: &Dinode) -> Vec<Dirent> {
        let Some(block) = self.plaintext.get(dir.direct[0].block as usize) else {
            return Vec::new();
        };
        let limit = (dir.size as usize).min(block.len());
        let mut out = Vec::new();
        let mut o = 0usize;
        while o + 16 <= limit {
            let ent_size = le32(block, o + 12) as usize;
            let name_len = le32(block, o + 8) as usize;
            if ent_size == 0 || o + 16 + name_len > limit {
                break;
            }
            out.push(Dirent {
                ino: le32(block, o),
                kind: i32le(block, o + 4),
                name: String::from_utf8_lossy(&block[o + 16..o + 16 + name_len]).into_owned(),
            });
            o += ent_size;
        }
        out
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg outer -- --nocapture`
Expected: 1 passed, no `skip:`.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src
git commit -m "feat(fpkg): decrypt and walk the outer PFS of a debug package"
```

---

### Task 6: `verify_package`, sample integration test, CLI

**Files:**
- Create: `src/verify.rs`, `tests/samples.rs`, `examples/fpkg_verify.rs`
- Modify: `src/lib.rs` (`pub mod verify;`)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub struct Check { pub name: String, pub ok: bool, pub detail: String }`
  - `pub struct Report { pub content_id: String, pub checks: Vec<Check> }` with `pub fn ok(&self) -> bool` and `impl std::fmt::Display`
  - `pub fn verify_package(path: &Path, passcode: &str) -> Result<Report>`

- [ ] **Step 1: Write the integration test**

`tests/samples.rs`:

```rust
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::verify::verify_package;

fn sample_dir() -> std::path::PathBuf {
    std::env::var("PS5UPLOAD_SAMPLE_PKGS")
        .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into())
        .into()
}

#[test]
fn every_check_passes_on_real_debug_samples() {
    let mut checked = 0;
    for name in [
        "webbrowser.pkg",
        "EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg",
    ] {
        let path = sample_dir().join(name);
        if !path.exists() {
            eprintln!("skip: {} not present", path.display());
            continue;
        }
        let report = verify_package(&path, DEFAULT_PASSCODE).unwrap();
        println!("{report}");
        assert!(report.ok(), "{name} failed:\n{report}");
        assert!(report.checks.len() >= 10, "{name}: too few checks ran");
        checked += 1;
    }
    eprintln!("verified {checked} real sample(s)");
}

#[test]
fn a_single_flipped_byte_fails_verification() {
    let src = sample_dir().join("EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg");
    let Ok(mut bytes) = std::fs::read(&src) else {
        eprintln!("skip: {} not present", src.display());
        return;
    };
    // Inside outer block 3 (the inode table): its imagedigs check must fail.
    bytes[0x10000 + 3 * 0x10000 + 100] ^= 0x01;
    let tmp = std::env::temp_dir().join(format!("fpkg-tamper-{}.pkg", std::process::id()));
    std::fs::write(&tmp, &bytes).unwrap();
    let report = verify_package(&tmp, DEFAULT_PASSCODE).unwrap();
    std::fs::remove_file(&tmp).ok();
    assert!(!report.ok());
    assert!(report
        .checks
        .iter()
        .any(|c| !c.ok && c.name.starts_with("outer block 3")));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd engine && cargo test -p ps5upload-fpkg --test samples`
Expected: compile error, `verify` module missing.

- [ ] **Step 3: Implement `verify.rs`**

```rust
//! Every integrity check we understand, as a named list.

use std::fmt;
use std::path::Path;

use crate::cnt::{self, EntryDigest};
use crate::crypto::sha3;
use crate::outer::{self, BlockKind};
use crate::{fih, si, PkgFile, Result};

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub struct Report {
    pub content_id: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    fn push(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            ok,
            detail: detail.into(),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.content_id)?;
        for c in &self.checks {
            let mark = if c.ok { "ok  " } else { "FAIL" };
            writeln!(f, "  [{mark}] {} {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

pub fn verify_package(path: &Path, passcode: &str) -> Result<Report> {
    let mut file = PkgFile::open(path)?;
    let head = file.read_at(0, fih::HEADER_LEN)?;
    let fih = fih::parse(&head)?;
    let cnt = cnt::read(&mut file, fih.cnt_offset)?;
    let mut r = Report {
        content_id: cnt.content_id.clone(),
        checks: Vec::new(),
    };

    r.push("fih debug image", fih.is_debug(), format!("signed byte {:#04x}", fih.signed_byte));
    r.push("fih format version 3", fih.format_version == 3, format!("{}", fih.format_version));
    r.push("cnt package digest", cnt.package_digest_ok(), "");
    r.push("cnt digest-table digest", cnt.digest_table_digest_ok(), "");
    for (id, verdict) in cnt.entry_digests() {
        r.push(
            format!("cnt entry {id:#06x} digest"),
            verdict != EntryDigest::Mismatch,
            format!("{verdict:?}"),
        );
    }
    if let Some(e) = cnt.entry(cnt::ids::PARAM_JSON) {
        let param = sha3(cnt.payload(e));
        let general = cnt.entry(0x0080).map(|g| cnt.payload(g)).unwrap_or(&[]);
        r.push(
            "param.json digest in GeneralDigests",
            general.windows(32).any(|w| w == param),
            "",
        );
    }

    let img = outer::open(&mut file, &fih, &cnt, passcode)?;
    r.push("outer superblock ICV", img.superblock.icv_ok, format!("block {}", img.superblock.index));
    for v in &img.verdicts {
        r.push(
            format!("outer block {} decrypts to its imagedigs entry", v.index),
            v.kind.is_some(),
            match v.kind {
                Some(BlockKind::Data) => "data sector",
                Some(BlockKind::Signed) => "signed sector",
                Some(BlockKind::Superblock) => "plaintext superblock",
                None => "no sector matched",
            },
        );
    }
    let table_ok = img
        .plaintext
        .get(img.superblock.inode_table_block as usize)
        .is_some_and(|b| sha3(b) == img.superblock.inode_table_digest);
    r.push("outer inode table digest", table_ok, "");
    let nodes = img.dinodes();
    for (ino, n) in nodes.iter().enumerate() {
        let ok = n
            .direct
            .iter()
            .take(n.blocks.min(12) as usize)
            .all(|d| img.plaintext.get(d.block as usize).is_some_and(|b| sha3(b) == d.digest));
        r.push(format!("outer inode {ino} block signatures"), ok, format!("{} block(s)", n.blocks));
    }
    let uroot: Vec<String> = nodes
        .get(2)
        .map(|n| img.dirents(n).into_iter().map(|d| d.name).collect())
        .unwrap_or_default();
    r.push(
        "outer uroot holds pfs_image.dat and naps_pkg_layout.dat",
        uroot.iter().any(|n| n == "pfs_image.dat") && uroot.iter().any(|n| n == "naps_pkg_layout.dat"),
        uroot.join(", "),
    );

    match si::read(&mut file)? {
        Some(s) => {
            let crc_name = format!("config/{}/playgo-chunk.crc", cnt.content_id);
            match s.members.iter().find(|m| m.name == crc_name) {
                Some(m) => {
                    let stored = file.read_at(m.offset, m.size as usize)?;
                    let expected = si::chunk_crc_table(&mut file, s.zip_start)?;
                    r.push("si playgo-chunk.crc", stored == expected, format!("{} bytes", m.size));
                }
                None => r.push("si playgo-chunk.crc", false, format!("{crc_name} missing")),
            }
        }
        None => r.push("si zip present", false, "no trailing STORED ZIP"),
    }
    Ok(r)
}
```

- [ ] **Step 4: CLI example**

`examples/fpkg_verify.rs`:

```rust
//! cargo run -p ps5upload-fpkg --example fpkg_verify -- <package> [passcode]

use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: fpkg_verify <package> [passcode]");
        std::process::exit(2);
    };
    let passcode = args.next().unwrap_or_else(|| DEFAULT_PASSCODE.to_string());
    match ps5upload_fpkg::verify::verify_package(std::path::Path::new(&path), &passcode) {
        Ok(report) => {
            print!("{report}");
            std::process::exit(if report.ok() { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
```

- [ ] **Step 5: Run everything**

Run: `cd engine && cargo fmt && cargo test -p ps5upload-fpkg -- --nocapture && cargo run -q -p ps5upload-fpkg --example fpkg_verify -- /Volumes/Storage/PS5/pkgs/webbrowser.pkg`
Expected: all tests pass with `verified 2 real sample(s)`; the example prints every check as `[ok  ]` and exits 0.

- [ ] **Step 6: Commit**

```bash
git add engine/crates/ps5upload-fpkg
git commit -m "feat(fpkg): verify_package recomputes every understood digest (gate G0)"
```

---

### Task 7: Gate

- [ ] **Step 1:** Run `cd engine && cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check --workspace --locked`. Expected: clean.
- [ ] **Step 2:** Record gate G0 as passed in the spec's verification section only if `every_check_passes_on_real_debug_samples` ran against both samples (not skipped). Commit the spec edit: `git add docs/superpowers/specs/2026-09-13-fpkg-builder-design.md && git commit -m "docs(fpkg): gate G0 passed on both debug samples"`.
- [ ] **Step 3:** Report to the user what G0 proved and what it did not (the inner image, `naps_meta_*`, RSA signature, FLT path hash), which is the scope of Plan 3.
