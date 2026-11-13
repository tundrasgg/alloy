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

pub fn os_random() -> u64 {
    rand::rngs::StdRng::from_os_rng().next_u64()
}

pub fn per_thread() {
    let _ = rand::rngs::ThreadRng::default();
}

pub fn pool() {
    let _ = tokio::runtime::Builder::new_multi_thread().build();
}

pub fn child() {
    let _ = std::process::Command::new("sh").spawn();
}

pub async fn async_child() {
    let _ = tokio::process::Command::new("sh").spawn();
}
