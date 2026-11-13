//! The simulator may call the operating system.

pub fn spawn() {
    std::thread::spawn(|| {});
}
