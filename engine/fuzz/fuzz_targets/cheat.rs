//! Local cheat import (`ps5upload_core::cheats::prepare_local_cheat`): a JSON trainer or an XML
//! game patch a user picked, checked and rewritten before it goes to the console.
//! Input: u8 kind (even: JSON, odd: XML), then the file's bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_core::cheats::prepare_local_cheat;

fuzz_target!(|data: &[u8]| {
    let Some((kind, bytes)) = data.split_first() else {
        return;
    };
    let name = if kind % 2 == 0 {
        "PPSA01234_01.000.json"
    } else {
        "PPSA01234_01.000.xml"
    };
    let _ = prepare_local_cheat(name, bytes);
});
