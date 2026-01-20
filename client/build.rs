use std::path::PathBuf;
use std::io::Write;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let chat_key_path = PathBuf::from("ps5upload_chat.key");
    let chat_key = std::fs::read_to_string(&chat_key_path).unwrap_or_default();
    let chat_key = chat_key.trim();
    let mut chat_key_file = std::fs::File::create(out_dir.join("chat_key.rs")).expect("Failed to write chat_key.rs");
    writeln!(
        chat_key_file,
        "const CHAT_SHARED_KEY_HEX: &str = \"{}\";",
        chat_key.escape_default()
    ).expect("Failed to write chat_key.rs");
    println!("cargo:rerun-if-changed={}", chat_key_path.display());

    let enable_unrar = std::env::var("CARGO_FEATURE_CLIENT_UNRAR").is_ok();
    if !enable_unrar {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unrar_dir = PathBuf::from("../payload/third_party/unrar");
    let mut sources = vec![
        "strlist.cpp",
        "strfn.cpp",
        "pathfn.cpp",
        "smallfn.cpp",
        "global.cpp",
        "file.cpp",
        "filefn.cpp",
        "filcreat.cpp",
        "archive.cpp",
        "arcread.cpp",
        "unicode.cpp",
        "system.cpp",
        "crypt.cpp",
        "crc.cpp",
        "rawread.cpp",
        "encname.cpp",
        "match.cpp",
        "timefn.cpp",
        "rdwrfn.cpp",
        "consio.cpp",
        "options.cpp",
        "errhnd.cpp",
        "rarvm.cpp",
        "secpassword.cpp",
        "rijndael.cpp",
        "getbits.cpp",
        "sha1.cpp",
        "sha256.cpp",
        "blake2s.cpp",
        "hash.cpp",
        "extinfo.cpp",
        "extract.cpp",
        "volume.cpp",
        "list.cpp",
        "find.cpp",
        "unpack.cpp",
        "headers.cpp",
        "rs16.cpp",
        "cmddata.cpp",
        "ui.cpp",
        "filestr.cpp",
        "scantree.cpp",
        "dll.cpp",
        "qopen.cpp",
        "largepage.cpp",
        "unrar_wrapper.cpp",
    ];

    if target_os == "windows" {
        sources.push("threadpool.cpp");
        sources.push("isnt.cpp");
        sources.push("motw.cpp");
    }

    let mut build = cc::Build::new();
    build.cpp(true);
    // Silence warnings from vendored unrar sources.
    build.warnings(false);
    build.flag_if_supported("-w");
    build.include(&unrar_dir);
    build.flag_if_supported("-std=c++11");
    build.define("SILENT", None);
    build.define("RARDLL", None);
    build.define("LITTLE_ENDIAN", None);
    build.define("ALLOW_MISALIGNED", None);
    build.define("_FILE_OFFSET_BITS", Some("64"));
    build.define("_LARGEFILE_SOURCE", None);
    build.flag_if_supported("-fno-rtti");

    if target_os == "macos" {
        let min_version = std::env::var("MACOSX_DEPLOYMENT_TARGET")
            .unwrap_or_else(|_| "10.12".to_string());
        build.flag(&format!("-mmacosx-version-min={}", min_version));
    }

    if !cfg!(target_os = "windows") {
        build.define("_UNIX", None);
    }

    for source in sources.iter() {
        let path = unrar_dir.join(source);
        build.file(&path);
        println!("cargo:rerun-if-changed={}", path.display());
    }

    build.compile("unrar_client");

    if target_os == "linux" || target_os == "android" {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    } else if target_os == "macos" || target_os == "ios" {
        println!("cargo:rustc-link-lib=dylib=c++");
    }
}
