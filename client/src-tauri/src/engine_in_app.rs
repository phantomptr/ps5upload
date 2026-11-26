//! Why the engine inside a phone app is not answering, in the words the renderer shows (#950).
//!
//! On a phone the engine is a task in the app's own process (`engine_mobile.rs`). When it does
//! not answer, the app used to show the socket error and nothing else. These say which of the
//! measured cases it is, as a coded error (`commands/shell_error.rs`) the renderer words in the
//! user's language (`client/src/lib/shellError.ts`); the log and a bug report keep the code and
//! the detail. Separate from `engine_mobile.rs` so the host tests cover it.

use crate::commands::shell_error::coded;

/// The engine did not start, or stopped: `detail` says why (the bind error, the panic, or the
/// last connection error).
pub(crate) const FAILED: &str = "engine_in_app_failed";

/// The engine holds its port, but the device refused the app's own connection to it.
pub(crate) const BLOCKED: &str = "engine_in_app_blocked";

/// The engine's server task ended: with an error from the engine, a panic, or by itself.
pub(crate) enum Ended {
    Error(String),
    Panic(String),
    Stopped,
}

/// The detail for a server task that ended.
pub(crate) fn ended_detail(ended: &Ended, bind: &str) -> String {
    match ended {
        Ended::Error(e) => e.clone(),
        Ended::Panic(p) => format!("it crashed on {bind}: {p}"),
        Ended::Stopped => format!("it stopped serving on {bind}"),
    }
}

/// A connection error that the device itself gave for 127.0.0.1, rather than an engine that is
/// not there: an engine that is down gives "Connection refused" (os error 61 on Apple systems,
/// 111 on Android). "No route to host" (EHOSTUNREACH, 65 on Apple systems, 113 on Android) to
/// loopback is the system declining the connection, as a VPN, a content filter or the system's
/// network policy does.
pub(crate) fn refused_by_device(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("no route to host")
        || d.contains("network is unreachable")
        || d.contains("os error 65)")
        || d.contains("os error 113)")
}

/// The coded error for an engine that does not answer: `detail` is the last connection error,
/// `port_held` whether something holds the engine's port (the engine's own listener, since the
/// app picked a free port for it).
pub(crate) fn not_answering(detail: &str, port_held: bool) -> String {
    let code = if port_held && refused_by_device(detail) {
        BLOCKED
    } else {
        FAILED
    };
    coded(code, serde_json::json!({ "detail": detail }))
}

/// The coded error for an engine whose server task ended.
pub(crate) fn ended(ended: &Ended, bind: &str) -> String {
    coded(
        FAILED,
        serde_json::json!({ "detail": ended_detail(ended, bind) }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    /// The error from #950: the device, not a missing engine.
    const NO_ROUTE: &str = "error sending request for url (http://127.0.0.1:19113/api/version): \
        client error (Connect): tcp connect error: No route to host (os error 65)";
    const REFUSED: &str = "error sending request for url (http://127.0.0.1:19113/api/version): \
        client error (Connect): tcp connect error: Connection refused (os error 61)";

    #[test]
    fn no_route_to_its_own_port_is_the_device_refusing() {
        assert!(refused_by_device(NO_ROUTE));
        assert!(refused_by_device(
            "tcp connect error: No route to host (os error 113)"
        ));
        assert!(!refused_by_device(REFUSED));
        assert!(!refused_by_device("operation timed out"));
        // A number that only starts like 65 is not EHOSTUNREACH.
        assert!(!refused_by_device("os error 650)"));

        let v = parse(&not_answering(NO_ROUTE, true));
        assert_eq!(v["code"], BLOCKED);
        assert_eq!(v["detail"], NO_ROUTE);
    }

    #[test]
    fn with_nothing_on_the_port_it_did_not_start() {
        // No listener: whatever the device said, the engine is not there.
        assert_eq!(parse(&not_answering(NO_ROUTE, false))["code"], FAILED);
        assert_eq!(parse(&not_answering(REFUSED, false))["code"], FAILED);
        // Its listener there and the connection refused or timing out: not the device.
        assert_eq!(parse(&not_answering(REFUSED, true))["code"], FAILED);
        assert_eq!(parse(&not_answering("timed out", true))["code"], FAILED);
    }

    #[test]
    fn an_ended_server_says_how_it_ended() {
        let bind = "0.0.0.0:19113";
        let e = Ended::Error(
            "in-process engine failed to bind 0.0.0.0:19113: Address already in use".into(),
        );
        let v = parse(&ended(&e, bind));
        assert_eq!(v["code"], FAILED);
        assert!(v["detail"]
            .as_str()
            .unwrap()
            .contains("Address already in use"));

        let v = parse(&ended(&Ended::Panic("no data folder".into()), bind));
        assert_eq!(v["detail"], "it crashed on 0.0.0.0:19113: no data folder");
        let v = parse(&ended(&Ended::Stopped, bind));
        assert_eq!(v["detail"], "it stopped serving on 0.0.0.0:19113");
    }
}
