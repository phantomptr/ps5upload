use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn healthcheck_uses_the_same_prefix_as_the_server() {
    for (base, path) in [
        ("/", "/api/jobs"),
        ("/ps5/", "/ps5/api/jobs"),
        ("ps5", "/ps5/api/jobs"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 1024];
            let n = stream.read(&mut bytes).unwrap();
            let correct = String::from_utf8_lossy(&bytes[..n])
                .starts_with(&format!("GET {path} HTTP/1.1\r\n"));
            let status = if correct { "200 OK" } else { "404 Not Found" };
            stream
                .write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .unwrap();
            correct
        });
        let result = Command::new(env!("CARGO_BIN_EXE_ps5upload-engine"))
            .arg("--healthcheck")
            .env("PS5UPLOAD_ENGINE_PORT", port.to_string())
            .env("PS5UPLOAD_BASE_URL", base)
            .status()
            .unwrap();
        assert!(server.join().unwrap(), "healthcheck ignored prefix {base}");
        assert!(result.success());
    }
}
