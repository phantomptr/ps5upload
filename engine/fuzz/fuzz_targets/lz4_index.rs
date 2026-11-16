//! AMPRIDX3 path index parser (`ampr_index::parse`) on arbitrary bytes. An index that parses
//! must build again from its own entries or fail cleanly.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::ampr_index::{build_entries, parse};

fuzz_target!(|data: &[u8]| {
    if let Some(entries) = parse(data) {
        let _ = build_entries(&entries);
    }
});
