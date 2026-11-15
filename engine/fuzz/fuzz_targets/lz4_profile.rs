//! LZ4 pack profiles (`ampr_pack::Config::from_toml`): the TOML a user can hand a build, with
//! an include reader that answers every list with the input itself.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::ampr_pack::Config;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut list = |_: &str| Ok(text.to_string());
    if let Ok(config) = Config::from_toml(text, Some(&mut list)) {
        let _ = config.canonical_json();
        let _ = config.select("data/a.bin");
        let _ = config.group_page("default");
    }
});
