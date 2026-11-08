//! Builds the C tailnet core (`tailnet/` in the repo root) with Monocypher (from the AVA1
//! submodule), BearSSL and lwIP (submodules) into one static library, for every target the
//! engine ships on: macOS, Linux, Windows (MSVC), Android and iOS. cc-rs picks the compiler
//! (cl.exe, clang, the NDK's clang) and its flags per target.

use std::path::{Path, PathBuf};

fn main() {
    // No canonicalize(): on Windows it returns a verbatim path (\\?\D:\...), which cl.exe behind
    // sccache cut down to '\\crypto.c' ("C1083: Cannot open source file"). Walk up instead.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("engine/crates/ps5upload-tailnet sits three levels below the repo root")
        .to_path_buf();
    let tn = root.join("tailnet");
    let third = root.join("third_party");
    let mono = third
        .join("ava1")
        .join("c")
        .join("third_party")
        .join("monocypher");
    let bearssl = third.join("bearssl");
    let lwip = third.join("lwip");
    for (what, p) in [
        ("tailnet core", under(&tn, "include/tailnet.h")),
        (
            "Monocypher (git submodule update --init third_party/ava1)",
            mono.join("monocypher.c"),
        ),
        (
            "BearSSL (git submodule update --init third_party/bearssl)",
            under(&bearssl, "inc/bearssl.h"),
        ),
        (
            "lwIP (git submodule update --init third_party/lwip)",
            under(&lwip, "src/include/lwip/tcp.h"),
        ),
    ] {
        assert!(p.exists(), "missing {what}: {}", p.display());
    }

    let target = std::env::var("TARGET").unwrap();
    let windows = target.contains("windows");
    if windows {
        // Where the C comes from, in the build log (the path bug above showed up only here).
        for p in [&tn, &mono, &bearssl, &lwip] {
            println!("cargo:warning=tailnet C source: {}", p.display());
        }
    }
    let msvc = target.contains("msvc");

    let mut b = cc::Build::new();
    b.std("c11")
        .warnings(false)
        .opt_level(2)
        .include(tn.join("include"))
        .include(tn.join("src"))
        .include(tn.join("lwip-port"))
        .include(&mono)
        .include(bearssl.join("inc"))
        .include(bearssl.join("src"))
        .include(under(&lwip, "src/include"))
        // Entropy is injected into every TLS engine by the core (src/net.c, src/derp.c).
        .define("BR_USE_URANDOM", "0")
        .define("BR_USE_GETENTROPY", "0")
        // lwIP asserts abort(); inside the engine a bad assert must not take the app down.
        .define("LWIP_NOASSERT", None);
    if msvc {
        b.define("_CRT_SECURE_NO_WARNINGS", None);
        b.define("_CRT_NONSTDC_NO_DEPRECATE", None);
    } else if !windows {
        b.define("_DEFAULT_SOURCE", None)
            .define("_DARWIN_C_SOURCE", None);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    for f in [
        "crypto.c",
        "json.c",
        "util.c",
        "keys.c",
        "net.c",
        "noise.c",
        "hpack.c",
        "wg.c",
        "disco.c",
        "ip.c",
        "derp.c",
        "dp.c",
        "fwd.c",
        "os.c",
        "h2.c",
        "netmap.c",
        "control.c",
        "tailnet.c",
    ] {
        files.push(tn.join("src").join(f));
    }
    files.push(mono.join("monocypher.c"));
    for f in [
        "init.c",
        "def.c",
        "inet_chksum.c",
        "ip.c",
        "mem.c",
        "memp.c",
        "netif.c",
        "pbuf.c",
        "tcp.c",
        "tcp_in.c",
        "tcp_out.c",
        "timeouts.c",
        "ipv4/ip4.c",
        "ipv4/ip4_addr.c",
    ] {
        files.push(under(&under(&lwip, "src/core"), f));
    }
    // All of BearSSL: the linker keeps only what the trimmed client profile references.
    collect_c(&bearssl.join("src"), &mut files);
    b.files(&files);
    b.compile("tailnet");

    if windows {
        for lib in ["ws2_32", "iphlpapi", "bcrypt", "advapi32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
    println!("cargo:rerun-if-changed={}", tn.join("src").display());
    println!("cargo:rerun-if-changed={}", tn.join("include").display());
    println!("cargo:rerun-if-changed={}", tn.join("lwip-port").display());
}

fn collect_c(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_c(&p, out);
        } else if p.extension().is_some_and(|e| e == "c") {
            out.push(p);
        }
    }
}

/// `base` joined with a '/'-separated relative path, one component at a time (native separators).
fn under(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, c| p.join(c))
}
