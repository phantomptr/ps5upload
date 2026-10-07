#![cfg(unix)]
//! The fan curve mapping (#354, #400). The PS5's ICC fan ioctl takes one temperature, the one the
//! firmware's fan control holds; lower is louder. The console's own value is 91 C (read back on
//! FW 13.60), not the 60 C this mapping used to cap at, so a curve maps to its lowest 100%-duty
//! point only when that is something we may set (45..=80 C); otherwise the fan goes back to the
//! console (0).
use ava1_ctest::fan_map_threshold as map;

fn curve(points: &[(i32, i32)]) -> String {
    let p: Vec<String> = points
        .iter()
        .map(|(t, d)| format!("{{\"temp_c\":{t},\"duty_pct\":{d}}}"))
        .collect();
    format!("{{\"points\":[{}]}}", p.join(","))
}

#[test]
fn the_reporters_curve_does_not_become_a_max_fans_target() {
    // First point 50 C / 30%: the old code sent 50. The next sent "stock 60", which is 31 C
    // below what the console uses by itself. It only reaches 100% at 85 C: leave it to the console.
    let t = map(&curve(&[(50, 30), (65, 55), (75, 80), (85, 100)]));
    assert_eq!(t, 0);
}

#[test]
fn a_100_percent_point_we_may_set_is_honoured() {
    assert_eq!(map(&curve(&[(40, 20), (55, 100), (70, 100)])), 55);
    assert_eq!(map(&curve(&[(60, 40), (80, 100)])), 80);
}

#[test]
fn lowest_full_duty_point_wins_even_if_unsorted() {
    assert_eq!(map(&curve(&[(70, 100), (52, 100), (45, 30)])), 52);
}

#[test]
fn floored_at_45_and_handed_back_to_the_console_above_80() {
    assert_eq!(map(&curve(&[(30, 100)])), 45);
    assert_eq!(map(&curve(&[(81, 100)])), 0);
    assert_eq!(map(&curve(&[(90, 100)])), 0);
    assert_eq!(map(&curve(&[(50, 10), (90, 60)])), 0); // never asks for 100%
}

#[test]
fn field_order_and_whitespace_do_not_matter() {
    assert_eq!(
        map("{\"points\":[{\"duty_pct\": 100, \"temp_c\": 58}]}"),
        58
    );
    assert_eq!(
        map("{\"points\":[{\"duty_pct\":20,\"temp_c\":50},{\"duty_pct\":100,\"temp_c\":57}]}"),
        57
    );
}

#[test]
fn unreadable_bodies_apply_nothing() {
    assert_eq!(map("{\"points\":[]}"), -1);
    assert_eq!(map(""), -1);
    assert_eq!(map("{\"temp_c\":abc}"), -1);
}
