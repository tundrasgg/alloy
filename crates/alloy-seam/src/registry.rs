//! The registry of implementations (ARFC 0001 clauses 3.1, 3.11, and 3.12).

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::{ConfigError, ConfigValue, Detail, ImplName, Kind, NameError, Seam, SwapClass};

/// Builds an instance of seam `S` from its context and configuration.
///
/// A factory validates the configuration and returns
/// [`BuildError::Config`] when it is rejected, or [`BuildError::Failed`] when
/// the instance cannot be built for another reason.
pub type Factory<S> = Arc<
    dyn Fn(&<S as Seam>::Context, &ConfigValue) -> Result<Arc<<S as Seam>::Object>, BuildError>
        + Send
        + Sync,
>;

/// Why an instance was not built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// No implementation `name` of `kind` is registered in this process.
    #[error("seam {kind}/{name} is not available on this node")]
    SeamUnavailable {
        /// The seam's kind.
        kind: Kind,
        /// The implementation asked for.
        name: ImplName,
    },
    /// The factory rejected the configuration.
    #[error("configuration rejected: {0}")]
    Config(ConfigError),
    /// The instance failed to build for a reason other than its configuration.
    /// Also returned by [`Registry::instantiate`] for a seam whose `KIND` is
    /// not a valid [`Kind`], which can never be registered.
    #[error("instance failed to build: {0}")]
    Failed(Detail),
}

/// Why a registration was refused. The builder is unchanged by a refused
/// registration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// The seam's `KIND` breaks the rules of [`Kind::new`].
    #[error("invalid kind {kind:?}: {error}")]
    InvalidKind {
        /// The seam's `KIND`, as declared.
        kind: String,
        /// The rule it breaks.
        error: NameError,
    },
    /// The implementation is already registered under this kind.
    #[error("{kind}/{name} is already registered")]
    Duplicate {
        /// The seam's kind.
        kind: Kind,
        /// The implementation name.
        name: ImplName,
    },
    /// `kind` is already registered by a different seam marker type.
    #[error("kind {kind} belongs to another seam")]
    KindConflict {
        /// The contested kind.
        kind: Kind,
    },
    /// The builder holds 1,024 registrations.
    #[error("the registry holds 1024 implementations")]
    Full,
}

/// One registration. The factory is a `Factory<S>` for the marker `S` whose
/// `TypeId` is `marker`, erased so that one map holds every seam.
#[derive(Clone)]
struct Entry {
    marker: TypeId,
    swap: SwapClass,
    factory: Arc<dyn Any + Send + Sync>,
}

/// Registrations by kind, then by name, and the marker that owns each kind.
#[derive(Clone, Default)]
struct Entries {
    by_name: BTreeMap<(Kind, ImplName), Entry>,
    owners: BTreeMap<Kind, TypeId>,
}

impl Entries {
    fn list(&self) -> Vec<(Kind, ImplName, SwapClass)> {
        self.by_name
            .iter()
            .map(|((kind, name), entry)| (kind.clone(), name.clone(), entry.swap))
            .collect()
    }
}

/// Writes each registration as `kind/name` with its swap class, never the
/// factories.
impl fmt::Debug for Entries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(
                self.by_name
                    .iter()
                    .map(|((kind, name), entry)| format!("{kind}/{name} ({:?})", entry.swap)),
            )
            .finish()
    }
}

/// Collects registrations at process start (ARFC 0001 clause 3.1). Holds at
/// most 1,024 registrations (ARFC 0001 section 7).
#[derive(Debug, Default)]
pub struct RegistryBuilder(Entries);

impl RegistryBuilder {
    /// The most registrations one registry holds (ARFC 0001 section 7).
    const MAX_ENTRIES: usize = 1_024;

    /// An empty builder.
    pub fn new() -> RegistryBuilder {
        RegistryBuilder::default()
    }

    /// Registers `factory` as implementation `name` of seam `S`, under
    /// `S::KIND` and with `S::SWAP` (ARFC 0001 clause 3.11).
    ///
    /// # Errors
    ///
    /// Checked in this order, and the builder is unchanged on any of them:
    /// [`RegistryError::InvalidKind`] when `S::KIND` breaks the rules of
    /// [`Kind::new`], [`RegistryError::KindConflict`] when a marker type other
    /// than `S` registered `S::KIND` before, [`RegistryError::Duplicate`] when
    /// `(S::KIND, name)` is registered, and [`RegistryError::Full`] when the
    /// builder holds 1,024 registrations.
    pub fn register<S: Seam>(
        &mut self,
        name: ImplName,
        factory: Factory<S>,
    ) -> Result<(), RegistryError> {
        let kind = Kind::new(S::KIND).map_err(|error| RegistryError::InvalidKind {
            kind: S::KIND.to_owned(),
            error,
        })?;
        let marker = TypeId::of::<S>();
        if self
            .0
            .owners
            .get(&kind)
            .is_some_and(|owner| *owner != marker)
        {
            return Err(RegistryError::KindConflict { kind });
        }
        let key = (kind, name);
        if self.0.by_name.contains_key(&key) {
            let (kind, name) = key;
            return Err(RegistryError::Duplicate { kind, name });
        }
        if self.0.by_name.len() >= Self::MAX_ENTRIES {
            return Err(RegistryError::Full);
        }
        self.0.owners.insert(key.0.clone(), marker);
        self.0.by_name.insert(
            key,
            Entry {
                marker,
                swap: S::SWAP,
                factory: Arc::new(factory),
            },
        );
        Ok(())
    }

    /// Freezes the registrations into an immutable [`Registry`].
    pub fn build(self) -> Registry {
        Registry(Arc::new(self.0))
    }
}

/// The immutable set of registered implementations. Cheap to clone: clones
/// share the registrations.
#[derive(Clone, Debug)]
pub struct Registry(Arc<Entries>);

impl Registry {
    /// Builds an instance of implementation `name` of seam `S` by calling, once,
    /// exactly the factory that marker `S` registered under `(S::KIND, name)`,
    /// and returns its result unchanged (ARFC 0001 clause 3.12). There is no
    /// fallback to another implementation.
    ///
    /// # Errors
    ///
    /// [`BuildError::SeamUnavailable`] with `S::KIND` and `name`, calling no
    /// factory, when marker `S` registered no such implementation.
    /// [`BuildError::Failed`], calling no factory, when `S::KIND` is not a
    /// valid [`Kind`]. Otherwise whatever the factory returns.
    pub fn instantiate<S: Seam>(
        &self,
        context: &S::Context,
        name: &ImplName,
        config: &ConfigValue,
    ) -> Result<Arc<S::Object>, BuildError> {
        let kind = Kind::new(S::KIND).map_err(|error| {
            BuildError::Failed(Detail::new(&format!(
                "seam kind {:?} is invalid: {error}",
                S::KIND
            )))
        })?;
        let key = (kind, name.clone());
        let factory = self
            .0
            .by_name
            .get(&key)
            .filter(|entry| entry.marker == TypeId::of::<S>())
            .and_then(|entry| entry.factory.downcast_ref::<Factory<S>>());
        match factory {
            Some(factory) => factory(context, config),
            None => {
                let (kind, name) = key;
                Err(BuildError::SeamUnavailable { kind, name })
            }
        }
    }

    /// True when implementation `name` of `kind` is registered.
    pub fn contains(&self, kind: &Kind, name: &ImplName) -> bool {
        self.0.by_name.contains_key(&(kind.clone(), name.clone()))
    }

    /// Every registration once, sorted by kind, then by name, each with the
    /// `SWAP` of the marker that registered it.
    pub fn entries(&self) -> Vec<(Kind, ImplName, SwapClass)> {
        self.0.list()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A test seam's object: remembers which factory built it and from what.
    #[derive(Debug, PartialEq, Eq)]
    struct Built {
        by: &'static str,
        context: u32,
        version: u16,
    }

    #[derive(Debug)]
    struct Alpha;
    impl Seam for Alpha {
        type Object = Built;
        type Context = u32;
        const KIND: &'static str = "alpha";
        const SWAP: SwapClass = SwapClass::Drain;
    }

    /// Claims `alpha` too, from a different marker type.
    #[derive(Debug)]
    struct Impostor;
    impl Seam for Impostor {
        type Object = Built;
        type Context = u32;
        const KIND: &'static str = "alpha";
        const SWAP: SwapClass = SwapClass::Live;
    }

    #[derive(Debug)]
    struct Beta;
    impl Seam for Beta {
        type Object = Built;
        type Context = u32;
        const KIND: &'static str = "beta";
        const SWAP: SwapClass = SwapClass::Restart;
    }

    #[derive(Debug)]
    struct BadKind;
    impl Seam for BadKind {
        type Object = Built;
        type Context = u32;
        const KIND: &'static str = "Bad";
        const SWAP: SwapClass = SwapClass::Restart;
    }

    fn name(s: &str) -> ImplName {
        ImplName::new(s).unwrap()
    }

    fn kind(s: &str) -> Kind {
        Kind::new(s).unwrap()
    }

    fn factory<S>(by: &'static str, calls: Arc<AtomicUsize>) -> Factory<S>
    where
        S: Seam<Object = Built, Context = u32>,
    {
        Arc::new(move |context: &u32, config: &ConfigValue| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Built {
                by,
                context: *context,
                version: config.version(),
            }))
        })
    }

    fn quiet<S: Seam<Object = Built, Context = u32>>(by: &'static str) -> Factory<S> {
        factory::<S>(by, Arc::new(AtomicUsize::new(0)))
    }

    fn config(version: u16) -> ConfigValue {
        ConfigValue::defaults(version).unwrap()
    }

    #[test]
    fn instantiate_calls_exactly_the_registered_factory_once() {
        let mem_calls = Arc::new(AtomicUsize::new(0));
        let os_calls = Arc::new(AtomicUsize::new(0));
        let mut builder = RegistryBuilder::new();
        builder
            .register::<Alpha>(
                name("mem"),
                factory::<Alpha>("alpha/mem", mem_calls.clone()),
            )
            .unwrap();
        builder
            .register::<Alpha>(name("os"), factory::<Alpha>("alpha/os", os_calls.clone()))
            .unwrap();
        builder
            .register::<Beta>(name("mem"), quiet::<Beta>("beta/mem"))
            .unwrap();
        let registry = builder.build();

        let built = registry
            .instantiate::<Alpha>(&7, &name("os"), &config(3))
            .unwrap();
        assert_eq!(
            *built,
            Built {
                by: "alpha/os",
                context: 7,
                version: 3
            }
        );
        assert_eq!(os_calls.load(Ordering::SeqCst), 1);
        assert_eq!(mem_calls.load(Ordering::SeqCst), 0);

        let built = registry
            .instantiate::<Beta>(&1, &name("mem"), &config(1))
            .unwrap();
        assert_eq!(built.by, "beta/mem");
    }

    #[test]
    fn instantiate_returns_the_factory_error_unchanged() {
        let mut builder = RegistryBuilder::new();
        let failing: Factory<Alpha> = Arc::new(|_: &u32, _: &ConfigValue| {
            Err(BuildError::Config(ConfigError::Malformed {
                detail: Detail::new("field 1"),
            }))
        });
        builder.register::<Alpha>(name("mem"), failing).unwrap();
        let registry = builder.build();
        assert_eq!(
            registry.instantiate::<Alpha>(&0, &name("mem"), &config(1)),
            Err(BuildError::Config(ConfigError::Malformed {
                detail: Detail::new("field 1")
            }))
        );
    }

    #[test]
    fn missing_implementation_is_unavailable_and_calls_nothing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut builder = RegistryBuilder::new();
        builder
            .register::<Alpha>(name("mem"), factory::<Alpha>("alpha/mem", calls.clone()))
            .unwrap();
        let registry = builder.build();
        assert_eq!(
            registry.instantiate::<Alpha>(&0, &name("os"), &config(1)),
            Err(BuildError::SeamUnavailable {
                kind: kind("alpha"),
                name: name("os")
            })
        );
        assert_eq!(
            registry.instantiate::<Beta>(&0, &name("mem"), &config(1)),
            Err(BuildError::SeamUnavailable {
                kind: kind("beta"),
                name: name("mem")
            })
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            registry.instantiate::<BadKind>(&0, &name("mem"), &config(1)),
            Err(BuildError::Failed(_))
        ));
    }

    #[test]
    fn register_refuses_in_the_specified_order_and_leaves_the_builder_unchanged() {
        let mut builder = RegistryBuilder::new();
        assert_eq!(
            builder.register::<BadKind>(name("mem"), quiet::<BadKind>("bad")),
            Err(RegistryError::InvalidKind {
                kind: "Bad".to_owned(),
                error: NameError::InvalidByte { index: 0 }
            })
        );
        builder
            .register::<Alpha>(name("mem"), quiet::<Alpha>("first"))
            .unwrap();
        // The impostor's (alpha, mem) is also a duplicate; the conflict wins.
        assert_eq!(
            builder.register::<Impostor>(name("mem"), quiet::<Impostor>("impostor")),
            Err(RegistryError::KindConflict {
                kind: kind("alpha")
            })
        );
        assert_eq!(
            builder.register::<Impostor>(name("other"), quiet::<Impostor>("impostor")),
            Err(RegistryError::KindConflict {
                kind: kind("alpha")
            })
        );
        assert_eq!(
            builder.register::<Alpha>(name("mem"), quiet::<Alpha>("second")),
            Err(RegistryError::Duplicate {
                kind: kind("alpha"),
                name: name("mem")
            })
        );
        let registry = builder.build();
        assert_eq!(
            registry.entries(),
            vec![(kind("alpha"), name("mem"), SwapClass::Drain)]
        );
        let built = registry
            .instantiate::<Alpha>(&0, &name("mem"), &config(1))
            .unwrap();
        assert_eq!(built.by, "first");
        assert!(matches!(
            registry.instantiate::<Impostor>(&0, &name("mem"), &config(1)),
            Err(BuildError::SeamUnavailable { .. })
        ));
    }

    #[test]
    fn register_stops_at_1024_entries() {
        let mut builder = RegistryBuilder::new();
        for i in 0..RegistryBuilder::MAX_ENTRIES {
            let n = name(&format!("impl-{i}"));
            builder.register::<Alpha>(n, quiet::<Alpha>("a")).unwrap();
        }
        assert_eq!(
            builder.register::<Beta>(name("mem"), quiet::<Beta>("b")),
            Err(RegistryError::Full)
        );
        // A duplicate is reported as such even when the builder is full.
        assert_eq!(
            builder.register::<Alpha>(name("impl-0"), quiet::<Alpha>("a")),
            Err(RegistryError::Duplicate {
                kind: kind("alpha"),
                name: name("impl-0")
            })
        );
        let registry = builder.build();
        assert_eq!(registry.entries().len(), 1_024);
        assert!(!registry.contains(&kind("beta"), &name("mem")));
    }

    #[test]
    fn entries_are_sorted_by_kind_then_name_with_their_swap_class() {
        let mut builder = RegistryBuilder::new();
        builder
            .register::<Beta>(name("os"), quiet::<Beta>("b"))
            .unwrap();
        builder
            .register::<Alpha>(name("zeta"), quiet::<Alpha>("a"))
            .unwrap();
        builder
            .register::<Beta>(name("mem"), quiet::<Beta>("b"))
            .unwrap();
        builder
            .register::<Alpha>(name("mem"), quiet::<Alpha>("a"))
            .unwrap();
        let registry = builder.build();
        assert_eq!(
            registry.entries(),
            vec![
                (kind("alpha"), name("mem"), SwapClass::Drain),
                (kind("alpha"), name("zeta"), SwapClass::Drain),
                (kind("beta"), name("mem"), SwapClass::Restart),
                (kind("beta"), name("os"), SwapClass::Restart),
            ]
        );
        assert!(registry.contains(&kind("alpha"), &name("zeta")));
        assert!(!registry.contains(&kind("alpha"), &name("os")));
        assert!(!registry.contains(&kind("gamma"), &name("mem")));
        let clone = registry.clone();
        assert_eq!(clone.entries(), registry.entries());
    }

    #[test]
    fn several_instances_of_one_seam_coexist() {
        let mut builder = RegistryBuilder::new();
        builder
            .register::<Alpha>(name("mem"), quiet::<Alpha>("m"))
            .unwrap();
        builder
            .register::<Alpha>(name("os"), quiet::<Alpha>("o"))
            .unwrap();
        let registry = builder.build();
        let built: Vec<_> = [("mem", 1), ("mem", 2), ("os", 3)]
            .into_iter()
            .map(|(n, c)| {
                registry
                    .instantiate::<Alpha>(&c, &name(n), &config(1))
                    .unwrap()
            })
            .collect();
        let seen: Vec<_> = built.iter().map(|b| (b.by, b.context)).collect();
        assert_eq!(seen, [("m", 1), ("m", 2), ("o", 3)]);
        assert!(!Arc::ptr_eq(&built[0], &built[1]));
    }

    #[test]
    fn debug_lists_registrations_without_factories() {
        let mut builder = RegistryBuilder::new();
        builder
            .register::<Beta>(name("mem"), quiet::<Beta>("b"))
            .unwrap();
        assert_eq!(
            format!("{builder:?}"),
            r#"RegistryBuilder(["beta/mem (Restart)"])"#
        );
        let registry = builder.build();
        assert_eq!(
            format!("{registry:?}"),
            r#"Registry(["beta/mem (Restart)"])"#
        );
    }
}
