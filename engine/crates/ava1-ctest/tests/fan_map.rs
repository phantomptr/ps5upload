#![cfg(unix)]
//! Issue #354: the fan curve must never become a "max fans" threshold. The PS5's ICC fan ioctl takes
//! one temperature, the point where the firmware goes to turbo (stock 60 C); a curve is mapped to
//! the lowest 100%-duty point, capped at stock and floored at 45 C.
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
    // first point 50 C / 30%: the old code sent 50 (turbo from 50 C). The curve only reaches
    // 100% at 85 C, which is above stock, so the console keeps its stock 60.
    let t = map(&curve(&[(50, 30), (65, 55), (75, 80), (85, 100)]));
    assert_eq!(t, 60);
    assert_ne!(t, 50);
}

#[test]
fn a_100_percent_point_below_stock_is_honoured() {
    assert_eq!(map(&curve(&[(40, 20), (55, 100), (70, 100)])), 55);
}

#[test]
fn lowest_full_duty_point_wins_even_if_unsorted() {
    assert_eq!(map(&curve(&[(70, 100), (52, 100), (45, 30)])), 52);
}

#[test]
fn clamped_to_the_floor_and_never_above_stock() {
    assert_eq!(map(&curve(&[(30, 100)])), 45);
    assert_eq!(map(&curve(&[(80, 100)])), 60);
    assert_eq!(map(&curve(&[(50, 10), (90, 60)])), 60); // never asks for 100%: stock
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
