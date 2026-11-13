# ARFC 0001: Runtime seams

- Status: accepted
- Depends on: 0000
- Crates: alloy-seam, alloy-rt
- Decisions: 0009

## 1. Purpose

Every subsystem boundary in Alloy is replaceable at runtime, from the first
commit. A seam lets a cluster pick, mix, and migrate between implementations per
node or per resource without rebuilding, and lets the simulator stand in for the
world.

## 2. Model

A **seam** is four things:

1. an object-safe Rust trait,
2. a registry kind, a short name such as `disk` or `hypervisor`,
3. a configuration schema for each implementation,
4. a conformance suite generic over the trait.

An **implementation** registers under `(kind, name)`. An **instance** is an
implementation built from configuration at runtime. Instances are chosen by
control plane state, for example a pool names its storage backend and a node names
its hypervisor.

A **binding** is an implementation name with a configuration value. A **slot**
holds the current instance of one seam for one user, together with its binding,
and is what the live swap protocol replaces.

### 2.1 Interface of `alloy-seam`

`alloy-seam` depends on no other Alloy crate. `alloy-rt` depends on it (ARFC
0005 section 2). The items below are the crate's whole public interface. Items
marked private are not part of the interface and are the developer's choice.
Every error type derives `Debug`, `Clone`, `PartialEq`, and `Eq`, and implements
`std::error::Error` and `Display` through `thiserror`.

```rust
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The return type of every asynchronous method on every seam trait.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A registry kind such as `disk`. 1 to 32 bytes of `[a-z0-9-]` (section 7).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kind(/* private */);

/// An implementation name such as `mem`. Same rules as `Kind`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImplName(/* private */);

impl Kind {
    pub fn new(name: &str) -> Result<Kind, NameError>;
    pub fn as_str(&self) -> &str;
}

impl ImplName {
    pub fn new(name: &str) -> Result<ImplName, NameError>;
    pub fn as_str(&self) -> &str;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("name is empty")]
    Empty,
    #[error("name is {len} bytes, the limit is 32")]
    TooLong { len: usize },
    #[error("byte {index} is not in [a-z0-9-]")]
    InvalidByte { index: usize },
}

/// How an instance of a seam is replaced (clause 3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapClass {
    Live,
    Drain,
    Restart,
}

/// Binds one seam's trait to its registry kind. Each seam defines one
/// zero-sized marker type that implements this trait.
pub trait Seam: 'static {
    /// The trait object, for example `dyn Clock`.
    type Object: ?Sized + Send + Sync + fmt::Debug + 'static;
    /// What a factory receives besides its configuration. The owning ARFC
    /// names it: `()` for the runtime seams, `alloy_rt::Runtime` otherwise.
    type Context: Send + Sync + 'static;
    /// The registry kind. Must satisfy the rules of `Kind`.
    const KIND: &'static str;
    const SWAP: SwapClass;
}

/// A seam of class `live`. Gives the swap protocol access to the
/// `Swappable` methods of an instance.
pub trait LiveSeam: Seam {
    fn swappable(instance: &Self::Object) -> &dyn Swappable;
}

/// Methods the live swap protocol calls (clause 3.9). The trait of every
/// `live` seam has `Swappable` as a supertrait.
pub trait Swappable: Send + Sync {
    /// Stops taking new work and finishes or flushes the work already taken.
    fn quiesce(&self) -> BoxFuture<'_, Result<(), StepError>>;
    /// Takes new work again after a swap failed before activation.
    fn resume(&self) -> BoxFuture<'_, Result<(), StepError>>;
    /// The instance's state in the seam's own format. Called only while
    /// quiesced.
    fn export_state(&self) -> BoxFuture<'_, Result<SeamState, StepError>>;
    /// Loads state exported by an instance of the same seam, of any
    /// implementation. Called once, before activation.
    fn import_state(&self, state: SeamState) -> BoxFuture<'_, Result<(), StepError>>;
    /// Releases everything the instance holds. Called once, last.
    fn retire(&self) -> BoxFuture<'_, Result<(), StepError>>;
}

/// State moved between instances by a live swap. The owning ARFC defines
/// `bytes` for each `version`.
#[derive(Clone, PartialEq, Eq)]
pub struct SeamState {
    pub version: u16,
    pub bytes: Vec<u8>,
}

/// Writes `SeamState { version: <version>, len: <bytes.len()> }`, never
/// the bytes (clause 3.18).
impl fmt::Debug for SeamState { /* ... */ }

/// A failure of one swap step, reported by an instance.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct StepError {
    pub detail: Detail,
}

/// Human-readable failure text. At most 1,024 bytes: `Detail::new`
/// truncates longer text at the last UTF-8 character boundary that fits.
/// Never holds secrets (I-22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail(/* private */);

impl Detail {
    pub fn new(text: &str) -> Detail;
    pub fn as_str(&self) -> &str;
}

impl fmt::Display for Detail { /* writes as_str() */ }

/// A typed, versioned configuration value in serialized form (section 4).
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigValue(/* private */);

/// Writes `ConfigValue { version: <version>, len: <payload length> }`,
/// never the payload (clause 3.18).
impl fmt::Debug for ConfigValue { /* ... */ }

impl ConfigValue {
    /// The largest serialized size, in bytes.
    pub const MAX_SERIALIZED: usize = 65_536;
    /// Fails with `TooLarge` when `2 + payload.len()` exceeds
    /// `MAX_SERIALIZED`, and otherwise with `InvalidVersion` when `version`
    /// is 0. The size check comes first.
    pub fn new(version: u16, payload: Vec<u8>) -> Result<ConfigValue, ConfigError>;
    /// A value with an empty payload: every field takes its default.
    pub fn defaults(version: u16) -> Result<ConfigValue, ConfigError>;
    pub fn version(&self) -> u16;
    pub fn payload(&self) -> &[u8];
    /// The section 4 encoding.
    pub fn to_bytes(&self) -> Vec<u8>;
    /// Parses the section 4 encoding, with the checks in the order section
    /// 4 gives. Total: never panics.
    pub fn from_bytes(bytes: &[u8]) -> Result<ConfigValue, ConfigError>;
}

/// An implementation's typed configuration.
pub trait SeamConfig: Sized {
    /// The newest version `to_value` writes and `from_value` reads.
    const VERSION: u16;
    /// The oldest version `to_value` writes and `from_value` reads.
    const MIN_VERSION: u16;
    /// Writes the configuration in format `version` (clause 3.13).
    fn to_value(&self, version: u16) -> Result<ConfigValue, ConfigError>;
    fn from_value(value: &ConfigValue) -> Result<Self, ConfigError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration is {len} bytes serialized, the limit is 65536")]
    TooLarge { len: usize },
    #[error("configuration is shorter than its 2-byte version")]
    Truncated,
    #[error("configuration version 0 is not valid")]
    InvalidVersion,
    #[error("configuration version {version} is outside {min}..={max}")]
    UnsupportedVersion { version: u16, min: u16, max: u16 },
    #[error("configuration payload does not decode: {detail}")]
    Malformed { detail: Detail },
    #[error("configuration field {field} is invalid: {detail}")]
    Invalid { field: Detail, detail: Detail },
    /// `to_value` was asked for a version that cannot hold the value of
    /// `field`.
    #[error("configuration field {field} cannot be written in version {version}")]
    NotExpressible { version: u16, field: Detail },
}

/// An implementation name and its configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: ImplName,
    pub config: ConfigValue,
}

/// Builds an instance of seam `S` from its context and configuration.
pub type Factory<S> = Arc<
    dyn Fn(&<S as Seam>::Context, &ConfigValue) -> Result<Arc<<S as Seam>::Object>, BuildError>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// No implementation `name` of `kind` is registered in this process.
    #[error("seam {kind}/{name} is not available on this node")]
    SeamUnavailable { kind: Kind, name: ImplName },
    #[error("configuration rejected: {0}")]
    Config(ConfigError),
    #[error("instance failed to build: {0}")]
    Failed(Detail),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid kind {kind:?}: {error}")]
    InvalidKind { kind: String, error: NameError },
    #[error("{kind}/{name} is already registered")]
    Duplicate { kind: Kind, name: ImplName },
    /// `kind` is already registered by a different seam marker type.
    #[error("kind {kind} belongs to another seam")]
    KindConflict { kind: Kind },
    #[error("the registry holds 1024 implementations")]
    Full,
}

/// Collects registrations at process start (clause 3.1).
#[derive(Debug, Default)]
pub struct RegistryBuilder(/* private */);

impl RegistryBuilder {
    pub fn new() -> RegistryBuilder;
    pub fn register<S: Seam>(
        &mut self,
        name: ImplName,
        factory: Factory<S>,
    ) -> Result<(), RegistryError>;
    pub fn build(self) -> Registry;
}

/// The immutable set of registered implementations. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Registry(/* private */);

impl Registry {
    pub fn instantiate<S: Seam>(
        &self,
        context: &S::Context,
        name: &ImplName,
        config: &ConfigValue,
    ) -> Result<Arc<S::Object>, BuildError>;
    pub fn contains(&self, kind: &Kind, name: &ImplName) -> bool;
    /// Every registration, sorted by kind, then by name.
    pub fn entries(&self) -> Vec<(Kind, ImplName, SwapClass)>;
}

/// The current instance of seam `S` for one user, and its binding.
#[derive(Debug)]
pub struct Slot<S: Seam>(/* private */);

impl<S: Seam> Slot<S> {
    pub fn new(binding: Binding, instance: Arc<S::Object>) -> Slot<S>;
    pub fn current(&self) -> Arc<S::Object>;
    pub fn binding(&self) -> Binding;
    /// 0 at creation, plus 1 at each activation.
    pub fn generation(&self) -> u64;
}

impl<S: LiveSeam> Slot<S> {
    /// Runs the live swap protocol to `to` (clause 3.9). `deadline` is a
    /// future that completes when the swap must give up, normally a sleep on
    /// the caller's clock.
    pub fn swap<'a>(
        &'a self,
        registry: &'a Registry,
        context: &'a S::Context,
        to: Binding,
        deadline: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<Swapped, SwapError>>;
}

/// A completed swap. The swap succeeded even when `retired` is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swapped {
    pub generation: u64,
    pub retired: Result<(), StepError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStep {
    Prepare,
    Quiesce,
    Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SwapError {
    #[error("a swap is already running on this slot")]
    Busy,
    #[error("a swap future was dropped before it completed; the slot no longer swaps")]
    Poisoned,
    #[error("the seam's swap class is not live")]
    NotLive,
    /// The new instance did not build. The old instance was never quiesced.
    #[error("prepare failed: {0}")]
    Prepare(BuildError),
    #[error("{step:?} failed: {cause}")]
    Failed {
        step: SwapStep,
        cause: StepError,
        resumed: Result<(), StepError>,
    },
    #[error("deadline reached during {step:?}")]
    Deadline {
        step: SwapStep,
        resumed: Result<(), StepError>,
    },
}
```

## 3. Behavior

3.1 `alloy-seam` provides the registry: `register(kind, name, factory)` at process
start, and `instantiate(kind, name, config) -> Arc<dyn Trait>` at any time. In the
interface of section 2.1 these are `RegistryBuilder::register::<S>` and
`Registry::instantiate::<S>`, where the seam marker `S` supplies the kind and the
trait.

3.2 Implementation choice must come from configuration or control plane state at
runtime. Cargo features must not select implementations. They may only exclude
code that cannot compile on a platform.

3.3 Several instances of one seam, of the same or different implementations, must
be able to run in one process at once.

3.4 Every seam must have an in-memory implementation, registered as `mem`, that
runs on the simulator. It is a real implementation and passes the same suite.

3.5 An implementation must not be registered in a release build until it passes
the seam's conformance suite in CI.

3.6 Seam traits on a hot path must be batch oriented: methods take and return
slices or vectors of work, so dispatch cost is per batch.

3.7 Seam traits must not expose implementation types. Configuration crosses the
seam as a typed, versioned, serializable value.

3.8 Each seam declares a swap class:

| Class | Meaning |
|---|---|
| `live` | An instance can be replaced under load by the swap protocol below |
| `drain` | A resource is moved from one instance to another online, then the old one is retired |
| `restart` | Changing it restarts one process. Guests keep running (I-17) |

3.9 Live swap protocol: `prepare(new)` builds and validates the new instance,
`quiesce(old)` stops intake and flushes, `transfer` moves state through the seam's
own export and import methods, `activate(new)`, `retire(old)`. Any step may fail
before `activate`, and the old instance then resumes. After `activate` there is no
automatic way back.

3.10 Names. `Kind::new` and `ImplName::new` must accept exactly the strings of 1
to 32 bytes drawn from `[a-z0-9-]`, and otherwise return `Empty`, `TooLong`, or
`InvalidByte` with the index of the first offending byte.

3.11 Registration. `RegistryBuilder::register::<S>` must fail, and leave the
builder unchanged, with:

- `InvalidKind` when `S::KIND` breaks the rules of clause 3.10,
- `KindConflict` when another marker type registered `S::KIND` before, compared by
  `TypeId`,
- `Duplicate` when `(S::KIND, name)` is registered,
- `Full` when the builder holds 1,024 registrations.

There is no global registry. A process, or a simulated host, builds its own.

3.12 Instantiation. `Registry::instantiate::<S>` must call exactly the factory
registered under `(S::KIND, name)` by marker `S`, once, and return its result
unchanged. When there is none it must return `SeamUnavailable` with that kind and
name and call no factory: there is no fallback to another implementation.
`entries` must list every registration once, sorted by kind and then by name, each
with the `SWAP` of its marker.

3.13 Configuration. `SeamConfig::from_value` must return `UnsupportedVersion`, with
`min` set to `MIN_VERSION` and `max` to `VERSION`, for a version outside that
range, and must decode every version from `MIN_VERSION` to `VERSION`.
`SeamConfig::to_value(v)` must write format `v` for every `v` from `MIN_VERSION` to
`VERSION`, and return `UnsupportedVersion` with the same `min` and `max` for any
other `v`. When the configuration holds a value that format `v` cannot carry, for
example a field added after `v` set to something other than its default, it must
return `NotExpressible` naming that field, and never drop the value silently. For
every configuration `c` and every such `v` where `to_value(v)` succeeds,
`from_value(&c.to_value(v)?)` must return `c`. A configuration with an empty
payload must decode to the implementation's defaults. Every implementation's ARFC
defines its payload, for each version, as a Protocol Buffers message (section 4).

Bindings are part of control plane state (section 2), so serialized configuration
values are persisted, replicated to other nodes, and read by other releases. Their
format changes follow ARFC 0017 clause 3.3:

- `MIN_VERSION` must be no newer than the oldest version the previous minor
  release writes, so that one release of skew works (I-18).
- A release that raises `VERSION` from `p` to `p + 1` keeps `MIN_VERSION` at or
  below `p`. Whoever writes a binding to control plane state calls `to_value(p)`
  until the upgrade is finalized (ARFC 0017 clause 3.7), and `to_value(p + 1)`
  only after. The control plane gives a configuration that fails with
  `NotExpressible` before finalization back to the caller as an error, and does
  not store it.

3.14 Slots. `current` must return the instance installed by `new` or by the most
recent activation, from any thread, without waiting for a swap in progress. A swap
does not change `current`, `binding`, or `generation` before step 4 of clause
3.15, and changes all three in one atomic step there.

3.15 Swap. `Slot::swap` does nothing when called: it only builds the future. A
swap future **starts** at its first poll, and it is **running** from then until it
completes. A future that is never polled has no effect on the slot, and dropping
it is not a drop under clause 3.16. Each swap future, once started, performs these
steps, in order:

0. At its first poll, in this order: if another swap future of the slot is
   running, return `Busy`. If the slot is poisoned (clause 3.16), return
   `Poisoned`. If `S::SWAP` is not `Live`, return `NotLive`. Nothing changes, and
   `deadline` is not polled.
1. Prepare: `registry.instantiate::<S>(context, &to.name, &to.config)`. On failure
   return `Prepare` with the build error. The old instance is untouched.
2. Quiesce: `quiesce` on the old instance.
3. Transfer: `export_state` on the old instance, then `import_state` with that
   state on the new one.
4. Activate: install the new instance and `to` in the slot, add 1 to the
   generation.
5. Retire: `retire` on the old instance. Return `Swapped` with the new generation
   and the result of `retire`.

If step 2 or 3 fails, the swap calls `resume` on the old instance and then
`retire` on the new one, and returns `Failed` with the step, the cause, and the
result of `resume`. From step 1 until step 4 starts, every poll of the swap future
polls `deadline` before it advances the running step. In the first poll this
happens after step 0 and before step 1, so a deadline that has already completed
returns `Deadline` with step `Prepare` and calls no factory. If `deadline`
completes before step 4 starts, the swap stops the running step by dropping its
future, does the same cleanup, and returns
`Deadline` with the step and the result of `resume`; a deadline during step 1
leaves the old instance untouched and `resumed` is `Ok(())`. `resume` and the
cleanup `retire` are not bounded by `deadline`. After step 4 the deadline is
ignored.

3.16 A swap future dropped while it is running (clause 3.15) poisons the slot:
`current`, `binding`, and `generation` keep the values they had when it was
dropped, and every swap future that starts later returns `Poisoned`. Dropping a
future that has completed, or that was never polled, does not poison the slot.
Recovery is the owning subsystem's business, for example by building a new slot.

3.17 While an instance is quiesced, and after it is retired, calls on it either
wait or fail with the seam's retryable error. The owning ARFC says which, for each
method.

3.18 Debug output. The `Debug` output of `SeamState` and `ConfigValue` is exactly
the text their doc comments in section 2.1 give, and holds no state or payload
bytes, because live seams such as `key-provider` move key material through them
(I-22). `Binding`, `Slot`, and every other type that contains them inherit this.
The `Debug` output of a seam instance is its implementation's, and must not hold
secrets either.

## 4. Formats

A serialized configuration value is:

| Bytes | Field | Meaning |
|---|---|---|
| 0 to 1 | `version` | Unsigned 16-bit little endian, 1 or more |
| 2 to end | `payload` | The Protocol Buffers (proto3) binary encoding of the implementation's configuration message for `version` |

The whole value is at most 65,536 bytes, so the payload is at most 65,534 bytes.
An empty payload means every field takes its default. A value with a version of 0
is invalid. `ConfigValue::from_bytes` checks in this order and returns the first
error that applies: `Truncated` for fewer than 2 bytes, `TooLarge` for more than
65,536, and `InvalidVersion` for version 0. It does not decode the payload:
`SeamConfig::from_value` does.

## 5. Seams

The initial set. Each owning ARFC defines the trait.

| Kind | Owner | Swap class | First implementations |
|---|---|---|---|
| `clock`, `net`, `disk-io`, `rng`, `spawn` | 0005 | restart | `os`, `mem` (simulator) |
| `underlay` | 0004 | live | `udp`, `stream`, `mem` |
| `portmap` | 0004 | live | `none`, `pcp`, `mem` |
| `log-store`, `state-store` | 0006 | restart | `native`, `mem` |
| `authn` | 0007 | live | `local`, `oidc`, `mem` |
| `key-provider` | 0007 | live | `cluster`, `mem` |
| `attachment` | 0010 | drain | `anchor`, `l2`, `routed`, `bgp`, `mem` |
| `hypervisor` | 0011 | drain | `cloud-hypervisor`, `mem` |
| `sandbox` | 0011 | drain | `namespace`, `mem` |
| `scheduler-policy` | 0011 | live | `default`, `mem` |
| `disk` | 0012 | drain | `extent`, `file`, `mem` |
| `chunk-store` | 0012 | drain | `native`, `mem` |
| `block-backend` | 0013 | drain | `native`, `mem` |
| `blob-backend` | 0014 | drain | `native`, `mem` |
| `index-store` | 0014 | drain | `native`, `mem` |
| `metrics-sink`, `audit-sink` | 0018 | live | `native`, `otlp`, `mem` |

Adding a seam or an implementation is a spec change to the owning ARFC and to this
table.

Each seam marker's `Context` is `alloy_rt::Runtime` unless the owning ARFC states
otherwise. The trait of every `live` seam has `Swappable` as a supertrait and its
marker implements `LiveSeam`.

The registry and the slot are not a seam. Their conformance suite is
`tests/conformance/seam/`, and it checks clauses 3.10 to 3.18 and section 4 with
test seams and test configurations defined in the suite.

## 6. Failure behavior

An instance that fails to build from its configuration must leave the previous
instance serving and must surface the error on the resource that asked for it. A
resource bound to an implementation the node does not have must report
`SeamUnavailable` and must not fall back to a different implementation.

A configuration that the version a writer must use cannot carry fails with
`NotExpressible` and is not stored (clause 3.13).

A failed or timed-out swap leaves the old instance current and resumed, or reports
in `resumed` that it could not resume. The owning subsystem surfaces either result
on the resource whose binding was being changed.

## 7. Limits

| Bound | Value | At the bound |
|---|---|---|
| Registry name (`Kind`, `ImplName`) | 1 to 32 bytes, `[a-z0-9-]` | `NameError` |
| Serialized configuration value | 65,536 bytes | `ConfigError::TooLarge` |
| Registrations per registry | 1,024 | `RegistryError::Full` |
| `Detail` text | 1,024 bytes | Truncated at a character boundary |
| Concurrent swaps per slot | 1 | `SwapError::Busy` |

`SeamState` has no bound of its own: each live seam's ARFC states the largest
state its instances export.

## 8. Security

Implementations are compiled in. Loading code at runtime from outside the binary
is not supported. Upholds I-19.

Swap state crosses only between instances in one process. Configuration values
are persisted in control plane state and reach other nodes (clause 3.13), so a
configuration message never holds a secret in the clear: a secret an
implementation needs is named by reference, or stored encrypted under a key from
the `key-provider` seam, as ARFC 0007 clause 3.8 does for access key secrets.
Neither configuration values nor swap state appear in debug output (clause 3.18). Error details are text for operators and
must not carry secrets (I-22).

## 9. Open questions

None.
