//! The simulator crate: it may reach the operating system too.

// `alloy-sim` implements the runtime seams for the simulator, so it may call
// the operating system (ARFC 0005 clause 3.1.1).
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "alloy-sim implements the runtime seams for the simulator"
)]

/// Reads the wall clock.
pub fn now() -> std::time::Instant {
    std::time::Instant::now()
}

/// Seeds from the operating system.
pub fn seeded() -> rand::rngs::StdRng {
    use rand::SeedableRng;
    rand::rngs::StdRng::from_os_rng()
}

#[cfg(test)]
mod tests {
    #[tokio::test(flavor = "multi_thread")]
    async fn runs_on_a_pool() {
        assert!(super::now().elapsed().as_secs() < 60);
    }
}
