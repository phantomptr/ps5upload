//! `param.json` and `param.sfo` on arbitrary bytes, through every parser the app has for them:
//! the folder inspector's (`ps5upload_core::game_meta`), the package inspector's
//! (`ps5upload_pkg::sfo_params`) and the image builder's readers and rewrites
//! (`ps5upload_fpkg::source`, `ps4_param::param_json`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_core::game_meta;
use ps5upload_fpkg::{ps4_param, source};

fuzz_target!(|data: &[u8]| {
    let _ = game_meta::parse_param_json_bytes(data);
    let _ = game_meta::parse_param_sfo_bytes(data);
    let _ = game_meta::parse_sfo_string_keys(data);
    if let Ok(params) = ps5upload_pkg::sfo_params(data) {
        let _ = ps4_param::param_json(&params, "CUSA00000");
    }
    let _ = source::content_id(data);
    let _ = source::content_version_word(data);
    let _ = source::firmware_rewrite(data, "05.00");
    let _ = source::drm_rewrite(data);
    let _ = source::launch_rewrite(data);
    let _ = source::content_id_rewrite(data, "UP0000-PPSA00000_00-0000000000000000");
});
