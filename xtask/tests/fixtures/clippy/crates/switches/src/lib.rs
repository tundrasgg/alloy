//! Every way the T-0046 review found to switch the direct-io lints off in
//! source, and the other ways to write a lint level. Under the gate's
//! `clippy-forbid` step, rustc rejects each one as incompatible with the
//! forbid (E0453), so none of them lowers the ban.

// A `warn` of the lint, with `warn(warnings)` so that `-D warnings` does not
// raise it again.
#![warn(warnings)]
#![warn(clippy::disallowed_methods)]

/// The lint named by a raw identifier.
#[allow(clippy::r#disallowed_methods, reason = "a fixture case")]
pub fn raw() -> std::time::Instant {
    std::time::Instant::now()
}

macro_rules! quiet {
    ($lint:path, $body:item) => {
        #[allow($lint, reason = "a fixture case")]
        $body
    };
}

// The lint passed to a macro that builds the `allow`.
quiet!(
    clippy::disallowed_methods,
    pub fn built() -> std::time::Instant {
        std::time::Instant::now()
    }
);

macro_rules! level {
    ($level:ident) => {
        #[$level(clippy::disallowed_types, reason = "a fixture case")]
        pub fn leveled() -> bool {
            std::fs::File::open("x").is_ok()
        }
    };
}

// The level passed to a macro.
level!(expect);

macro_rules! attribute {
    ($meta:meta) => {
        #[$meta]
        pub fn whole() -> std::time::SystemTime {
            std::time::SystemTime::now()
        }
    };
}

// The whole attribute passed to a macro, as a lint group.
attribute!(allow(clippy::style, reason = "a fixture case"));

// An `allow` in a file that is not `.rs`, pulled in as source.
include!("included.txt");

/// An `allow` of the widest group, inside `cfg_attr`.
#[cfg_attr(all(), allow(clippy::all, reason = "a fixture case"))]
pub fn conditional() -> std::time::Instant {
    std::time::Instant::now()
}
