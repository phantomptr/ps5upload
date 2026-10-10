//! A second C server for the console-to-console tests (tests/c2c.rs). The C server is a
//! process-wide singleton, so the other console runs here, in its own process: it prints
//! `PORT <n>` and serves until its stdin closes.
use std::io::{BufRead, Write};
use std::path::PathBuf;

use ava1_ctest::CServer;

fn main() {
    let mut args = std::env::args().skip(1);
    let secret = [args
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .expect("secret byte"); 32];
    let peers = PathBuf::from(args.next().expect("peers file"));
    let jobs = PathBuf::from(args.next().expect("jobs dir"));
    let fsync_delay_us = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let srv = CServer::start_data(secret, &peers, &jobs, 200, 2000, 2000, fsync_delay_us);
    println!("PORT {}", srv.addr().rsplit(':').next().unwrap());
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    while std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map(|n| n > 0)
        .unwrap_or(false)
    {
        line.clear();
    }
    drop(srv);
}
