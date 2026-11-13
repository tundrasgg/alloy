//! Runtime seams: the registry of replaceable subsystem implementations and the
//! slots that swap them live (ARFC 0001).
//!
//! A **seam** is an object-safe trait, a registry [`Kind`], a configuration
//! schema per implementation ([`SeamConfig`]), and a conformance suite. Each
//! seam defines a zero-sized marker type implementing [`Seam`] that ties its
//! trait object to its kind and swap class.
//!
//! A process, or a simulated host, collects implementations in a
//! [`RegistryBuilder`] at start, freezes them into a [`Registry`], and builds
//! instances from [`Binding`]s chosen by configuration or control plane state.
//! There is no global registry. A [`Slot`] holds the current instance of one
//! seam for one user, and for `live` seams replaces it with the swap protocol of
//! ARFC 0001 clause 3.15.
//!
//! This crate depends on no other Alloy crate and performs no I/O.

#![forbid(unsafe_code)]

use std::fmt;
use std::future::Future;
use std::pin::Pin;

mod config;
mod detail;
mod name;
mod registry;
mod slot;

pub use config::{Binding, ConfigError, ConfigValue, SeamConfig};
pub use detail::Detail;
pub use name::{ImplName, Kind, NameError};
pub use registry::{BuildError, Factory, Registry, RegistryBuilder, RegistryError};
pub use slot::{Slot, SwapError, SwapStep, Swapped};

/// The return type of every asynchronous method on every seam trait: a boxed,
/// `Send` future, which keeps the traits object safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How an instance of a seam is replaced (ARFC 0001 clause 3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapClass {
    /// An instance can be replaced under load by the live swap protocol
    /// ([`Slot::swap`]).
    Live,
    /// A resource is moved from one instance to another online, then the old
    /// instance is retired. The owning subsystem runs the move.
    Drain,
    /// Changing the instance restarts one process. Guests keep running (I-17).
    Restart,
}

/// Binds one seam's trait to its registry kind and swap class.
///
/// Each seam defines one zero-sized marker type that implements this trait.
/// The registry tells markers apart by `TypeId`, so two markers can never share
/// a kind in one registry (ARFC 0001 clause 3.11).
pub trait Seam: 'static {
    /// The trait object, for example `dyn Clock`.
    type Object: ?Sized + Send + Sync + fmt::Debug + 'static;
    /// What a factory receives besides its configuration. The owning ARFC names
    /// it: `()` for the runtime seams, `alloy_rt::Runtime` otherwise.
    type Context: Send + Sync + 'static;
    /// The registry kind. Must satisfy the rules of [`Kind::new`], or
    /// registration fails with [`RegistryError::InvalidKind`].
    const KIND: &'static str;
    /// The seam's swap class.
    const SWAP: SwapClass;
}

/// A seam of class [`SwapClass::Live`]. Gives the swap protocol access to the
/// [`Swappable`] methods of an instance.
pub trait LiveSeam: Seam {
    /// The instance's swap protocol methods. For a trait with `Swappable` as a
    /// supertrait this is the upcast `instance`.
    fn swappable(instance: &Self::Object) -> &dyn Swappable;
}

/// Methods the live swap protocol calls (ARFC 0001 clauses 3.9 and 3.15). The
/// trait of every `live` seam has `Swappable` as a supertrait.
///
/// While an instance is quiesced, and after it is retired, its other methods
/// either wait or fail with the seam's retryable error, as the owning ARFC says
/// for each method (clause 3.17).
pub trait Swappable: Send + Sync {
    /// Stops taking new work and finishes or flushes the work already taken.
    ///
    /// # Errors
    ///
    /// A [`StepError`] when the instance cannot quiesce. The swap then resumes
    /// it.
    fn quiesce(&self) -> BoxFuture<'_, Result<(), StepError>>;

    /// Takes new work again after a swap failed before activation.
    ///
    /// # Errors
    ///
    /// A [`StepError`] when the instance cannot resume. The swap reports it in
    /// the `resumed` field of its error.
    fn resume(&self) -> BoxFuture<'_, Result<(), StepError>>;

    /// The instance's state in the seam's own format. Called only while
    /// quiesced.
    ///
    /// # Errors
    ///
    /// A [`StepError`] when the state cannot be exported.
    fn export_state(&self) -> BoxFuture<'_, Result<SeamState, StepError>>;

    /// Loads state exported by an instance of the same seam, of any
    /// implementation. Called once, before activation.
    ///
    /// # Errors
    ///
    /// A [`StepError`] when the state cannot be loaded, for example an unknown
    /// `version`.
    fn import_state(&self, state: SeamState) -> BoxFuture<'_, Result<(), StepError>>;

    /// Releases everything the instance holds. Called once, last.
    ///
    /// # Errors
    ///
    /// A [`StepError`] when something could not be released. The swap reports
    /// it and is not undone by it.
    fn retire(&self) -> BoxFuture<'_, Result<(), StepError>>;
}

/// State moved between instances by a live swap. The owning ARFC defines
/// `bytes` for each `version` and the largest state its instances export.
///
/// Its `Debug` output never holds the bytes (ARFC 0001 clause 3.18), because
/// live seams such as `key-provider` move key material through it.
#[derive(Clone, PartialEq, Eq)]
pub struct SeamState {
    /// The format of `bytes`, defined by the owning ARFC.
    pub version: u16,
    /// The state, in the format of `version`.
    pub bytes: Vec<u8>,
}

/// Writes `SeamState { version: <version>, len: <bytes.len()> }`, never the
/// bytes (ARFC 0001 clause 3.18).
impl fmt::Debug for SeamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeamState")
            .field("version", &self.version)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// A failure of one swap step, reported by an instance.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct StepError {
    /// What failed, for operators. Never holds secrets (I-22).
    pub detail: Detail,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seam_state_debug_shows_version_and_length_only() {
        let state = SeamState {
            version: 4,
            bytes: b"key material".to_vec(),
        };
        assert_eq!(format!("{state:?}"), "SeamState { version: 4, len: 12 }");
        assert_eq!(
            format!("{state:#?}"),
            "SeamState {\n    version: 4,\n    len: 12,\n}"
        );
    }

    #[test]
    fn step_error_displays_its_detail() {
        let error = StepError {
            detail: Detail::new("flush failed"),
        };
        assert_eq!(error.to_string(), "flush failed");
    }
}
