//! Byte sizes in the words the engine shows people.
//!
//! One formatter, binary units labelled as such (KiB, MiB, GiB): the engine used to say "GB"
//! for both 10^9 and 2^30 bytes depending on the message, so two numbers about the same drive
//! could disagree by 7 %.

/// `512 B`, `1.5 KiB`, `2.0 GiB`: powers of 1024, one decimal past bytes.
pub fn iec_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::iec_bytes;

    #[test]
    fn sizes_use_binary_units_with_binary_labels() {
        assert_eq!(iec_bytes(0), "0 B");
        assert_eq!(iec_bytes(512), "512 B");
        assert_eq!(iec_bytes(1536), "1.5 KiB");
        assert_eq!(iec_bytes(2 << 30), "2.0 GiB");
        assert_eq!(iec_bytes(5 * (1 << 20)), "5.0 MiB");
        assert_eq!(iec_bytes(3 << 40), "3.0 TiB");
    }
}
