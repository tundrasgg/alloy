//! One reach for each kind of operating system resource.

use std::fs;

pub fn clock() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

pub fn network(addr: std::net::SocketAddr) {
    let _ = UdpSocket::bind(addr);
}

pub fn file() {
    let _ = fs::read("x");
}

pub fn thread() {
    tokio::spawn(async {});
}

pub fn random() -> u8 {
    rand::thread_rng().r#gen()
}
