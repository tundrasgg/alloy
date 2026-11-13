//! Slots and the live swap protocol (ARFC 0001 clauses 3.9 and 3.14 to 3.16).

use std::fmt;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;

use crate::{
    Binding, BoxFuture, BuildError, LiveSeam, Registry, Seam, StepError, SwapClass, Swappable,
};

/// Locks `mutex`. The slot's locks are held only to copy or replace plain
/// values, never across a call that can panic, so a poisoned lock still holds
/// consistent data and is used as is.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a slot serves: changed only as a whole, at activation.
struct Current<S: Seam> {
    instance: Arc<S::Object>,
    binding: Binding,
    generation: u64,
}

/// The slot's swap bookkeeping (ARFC 0001 clauses 3.15 and 3.16).
#[derive(Debug, Default)]
struct SwapFlags {
    /// A swap future has started and not completed.
    running: bool,
    /// A running swap future was dropped.
    poisoned: bool,
}

/// The current instance of seam `S` for one user, and its binding (ARFC 0001
/// section 2).
///
/// [`Slot::current`], [`Slot::binding`], and [`Slot::generation`] may be called
/// from any thread and never wait for a swap in progress. They change only at
/// a swap's activation, all three in one atomic step (clause 3.14). At most one
/// swap runs on a slot at a time.
pub struct Slot<S: Seam> {
    current: Mutex<Current<S>>,
    swap: Mutex<SwapFlags>,
}

/// Writes the binding, which shows the configuration's version and length only
/// (clause 3.18), the generation, and the instance's own `Debug` output.
impl<S: Seam> fmt::Debug for Slot<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let current = lock(&self.current);
        f.debug_struct("Slot")
            .field("kind", &S::KIND)
            .field("binding", &current.binding)
            .field("generation", &current.generation)
            .field("instance", &current.instance)
            .finish()
    }
}

impl<S: Seam> Slot<S> {
    /// A slot serving `instance`, built from `binding`, at generation 0.
    pub fn new(binding: Binding, instance: Arc<S::Object>) -> Slot<S> {
        Slot {
            current: Mutex::new(Current {
                instance,
                binding,
                generation: 0,
            }),
            swap: Mutex::new(SwapFlags::default()),
        }
    }

    /// The instance installed by [`Slot::new`] or by the most recent
    /// activation.
    pub fn current(&self) -> Arc<S::Object> {
        Arc::clone(&lock(&self.current).instance)
    }

    /// The binding of [`Slot::current`].
    pub fn binding(&self) -> Binding {
        lock(&self.current).binding.clone()
    }

    /// 0 at creation, plus 1 at each activation.
    pub fn generation(&self) -> u64 {
        lock(&self.current).generation
    }

    /// Step 0 of clause 3.15: claims the slot for one swap future, or says why
    /// it cannot.
    fn start(&self) -> Result<Running<'_>, SwapError> {
        let mut flags = lock(&self.swap);
        if flags.running {
            return Err(SwapError::Busy);
        }
        if flags.poisoned {
            return Err(SwapError::Poisoned);
        }
        if S::SWAP != SwapClass::Live {
            return Err(SwapError::NotLive);
        }
        flags.running = true;
        Ok(Running {
            flags: &self.swap,
            completed: false,
        })
    }

    /// Step 4 of clause 3.15: installs `instance` and `binding` and returns the
    /// new generation.
    fn activate(&self, instance: Arc<S::Object>, binding: Binding) -> u64 {
        let mut current = lock(&self.current);
        current.instance = instance;
        current.binding = binding;
        current.generation = current.generation.saturating_add(1);
        current.generation
    }
}

/// Marks one swap future as running. Dropped before `completed` is set, it
/// poisons the slot (clause 3.16).
struct Running<'a> {
    flags: &'a Mutex<SwapFlags>,
    completed: bool,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut flags = lock(self.flags);
        flags.running = false;
        if !self.completed {
            flags.poisoned = true;
        }
    }
}

/// The outcome of a step raced against the deadline.
enum Raced<T> {
    Deadline,
    Done(T),
}

/// Polls `deadline`, then `step`, on every poll. When the deadline completes
/// first the step's future is dropped, which stops it.
async fn race<T>(deadline: &mut BoxFuture<'_, ()>, mut step: BoxFuture<'_, T>) -> Raced<T> {
    poll_fn(|cx| {
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Raced::Deadline);
        }
        step.as_mut().poll(cx).map(Raced::Done)
    })
    .await
}

/// Polls `deadline` once and reports whether it has completed.
async fn deadline_reached(deadline: &mut BoxFuture<'_, ()>) -> bool {
    poll_fn(|cx| Poll::Ready(deadline.as_mut().poll(cx).is_ready())).await
}

/// How a swap that got past step 1 ended before activation.
enum Abort {
    Failed(SwapStep, StepError),
    Deadline(SwapStep),
}

impl<S: LiveSeam> Slot<S> {
    /// Runs the live swap protocol to `to` (ARFC 0001 clauses 3.9 and 3.15).
    /// `deadline` is a future that completes when the swap must give up,
    /// normally a sleep on the caller's clock.
    ///
    /// Calling `swap` does nothing: the returned future starts at its first
    /// poll. Once started it prepares the new instance from `registry` and
    /// `context`, quiesces the old one, transfers its state, activates the new
    /// one, and retires the old one. The deadline is polled before the running
    /// step on every poll until activation, and ignored after. Dropping the
    /// future after it started and before it completed poisons the slot
    /// (clause 3.16).
    ///
    /// # Errors
    ///
    /// [`SwapError::Busy`], [`SwapError::Poisoned`], or [`SwapError::NotLive`]
    /// at the first poll, with nothing changed. [`SwapError::Prepare`] when the
    /// new instance does not build. [`SwapError::Failed`] or
    /// [`SwapError::Deadline`] when quiesce or transfer fails or the deadline
    /// completes before activation; the old instance is then resumed, the new
    /// one retired, and the slot unchanged.
    pub fn swap<'a>(
        &'a self,
        registry: &'a Registry,
        context: &'a S::Context,
        to: Binding,
        deadline: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<Swapped, SwapError>> {
        Box::pin(async move {
            let mut running = self.start()?;
            let result = self.run(registry, context, to, deadline).await;
            running.completed = true;
            result
        })
    }

    /// Steps 1 to 5 of clause 3.15, after step 0 has claimed the slot.
    async fn run(
        &self,
        registry: &Registry,
        context: &S::Context,
        to: Binding,
        mut deadline: BoxFuture<'_, ()>,
    ) -> Result<Swapped, SwapError> {
        if deadline_reached(&mut deadline).await {
            return Err(SwapError::Deadline {
                step: SwapStep::Prepare,
                resumed: Ok(()),
            });
        }
        let new = registry
            .instantiate::<S>(context, &to.name, &to.config)
            .map_err(SwapError::Prepare)?;
        let old = self.current();
        let abort = Self::transfer(S::swappable(&old), S::swappable(&new), &mut deadline).await;
        if let Err(abort) = abort {
            let resumed = S::swappable(&old).resume().await;
            // The swap has failed already; the new instance's retirement
            // result has no field to go in and changes nothing for the caller.
            let _ = S::swappable(&new).retire().await;
            return Err(match abort {
                Abort::Failed(step, cause) => SwapError::Failed {
                    step,
                    cause,
                    resumed,
                },
                Abort::Deadline(step) => SwapError::Deadline { step, resumed },
            });
        }
        let generation = self.activate(new, to);
        let retired = S::swappable(&old).retire().await;
        Ok(Swapped {
            generation,
            retired,
        })
    }

    /// Steps 2 and 3 of clause 3.15, each raced against the deadline.
    async fn transfer(
        old: &dyn Swappable,
        new: &dyn Swappable,
        deadline: &mut BoxFuture<'_, ()>,
    ) -> Result<(), Abort> {
        match race(deadline, old.quiesce()).await {
            Raced::Deadline => return Err(Abort::Deadline(SwapStep::Quiesce)),
            Raced::Done(Err(cause)) => return Err(Abort::Failed(SwapStep::Quiesce, cause)),
            Raced::Done(Ok(())) => {}
        }
        let state = match race(deadline, old.export_state()).await {
            Raced::Deadline => return Err(Abort::Deadline(SwapStep::Transfer)),
            Raced::Done(Err(cause)) => return Err(Abort::Failed(SwapStep::Transfer, cause)),
            Raced::Done(Ok(state)) => state,
        };
        match race(deadline, new.import_state(state)).await {
            Raced::Deadline => Err(Abort::Deadline(SwapStep::Transfer)),
            Raced::Done(Err(cause)) => Err(Abort::Failed(SwapStep::Transfer, cause)),
            Raced::Done(Ok(())) => Ok(()),
        }
    }
}

/// A completed swap. The swap succeeded even when `retired` is an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swapped {
    /// The slot's generation after activation.
    pub generation: u64,
    /// The result of `retire` on the old instance.
    pub retired: Result<(), StepError>,
}

/// The swap steps that can fail or run out of time before activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapStep {
    /// Building and validating the new instance.
    Prepare,
    /// Stopping intake on the old instance and flushing it.
    Quiesce,
    /// Exporting the old instance's state and importing it into the new one.
    Transfer,
}

/// Why a swap did not activate the new instance.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SwapError {
    /// Another swap future of the slot is running. Nothing changed.
    #[error("a swap is already running on this slot")]
    Busy,
    /// A running swap future of the slot was dropped earlier (clause 3.16).
    /// Nothing changed; the slot never swaps again.
    #[error("a swap future was dropped before it completed; the slot no longer swaps")]
    Poisoned,
    /// The seam's `SWAP` is not [`SwapClass::Live`]. Nothing changed.
    #[error("the seam's swap class is not live")]
    NotLive,
    /// The new instance did not build. The old instance was never quiesced.
    #[error("prepare failed: {0}")]
    Prepare(BuildError),
    /// Quiesce or transfer failed. The old instance was resumed and the new one
    /// retired.
    #[error("{step:?} failed: {cause}")]
    Failed {
        /// The step that failed: `Quiesce` or `Transfer`.
        step: SwapStep,
        /// The instance's error.
        cause: StepError,
        /// The result of `resume` on the old instance.
        resumed: Result<(), StepError>,
    },
    /// The deadline completed before activation. The running step was stopped,
    /// the old instance resumed, and any new instance retired.
    #[error("deadline reached during {step:?}")]
    Deadline {
        /// The step that was running.
        step: SwapStep,
        /// The result of `resume` on the old instance; `Ok(())` when the
        /// deadline came during `Prepare`, which leaves the old instance
        /// untouched.
        resumed: Result<(), StepError>,
    },
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Waker};

    use super::*;
    use crate::{ConfigValue, Detail, Factory, ImplName, RegistryBuilder, SeamState};

    /// What one test instance does for one swap method.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Act {
        Ok,
        Fail,
        Hang,
    }

    impl Act {
        fn from_byte(b: u8) -> Act {
            match b {
                1 => Act::Fail,
                2 => Act::Hang,
                _ => Act::Ok,
            }
        }
    }

    /// The acts of quiesce, resume, export, import, and retire, in that order.
    type Script = [Act; 5];
    const ALL_OK: Script = [Act::Ok; 5];

    /// The log of swap method calls that every test instance writes to.
    type Log = Arc<Mutex<Vec<String>>>;

    /// A test live seam.
    trait Probe: Swappable + fmt::Debug {
        fn label(&self) -> &str;
        fn imported(&self) -> Option<SeamState>;
    }

    #[derive(Debug)]
    struct ProbeSeam;
    impl Seam for ProbeSeam {
        type Object = dyn Probe;
        type Context = Log;
        const KIND: &'static str = "probe";
        const SWAP: SwapClass = SwapClass::Live;
    }
    impl LiveSeam for ProbeSeam {
        fn swappable(instance: &dyn Probe) -> &dyn Swappable {
            instance
        }
    }

    /// Implements `LiveSeam` but declares a non-live swap class.
    #[derive(Debug)]
    struct DrainProbeSeam;
    impl Seam for DrainProbeSeam {
        type Object = dyn Probe;
        type Context = Log;
        const KIND: &'static str = "drain-probe";
        const SWAP: SwapClass = SwapClass::Drain;
    }
    impl LiveSeam for DrainProbeSeam {
        fn swappable(instance: &dyn Probe) -> &dyn Swappable {
            instance
        }
    }

    #[derive(Debug)]
    struct Instance {
        label: String,
        script: Script,
        log: Log,
        imported: Mutex<Option<SeamState>>,
    }

    impl Instance {
        fn act<T: Send + 'static>(
            &self,
            method: &str,
            index: usize,
            ok: T,
        ) -> BoxFuture<'_, Result<T, StepError>> {
            lock(&self.log).push(format!("{}.{method}", self.label));
            let fail = StepError {
                detail: Detail::new(&format!("{}.{method} failed", self.label)),
            };
            match self.script[index] {
                Act::Ok => Box::pin(async move { Ok(ok) }),
                Act::Fail => Box::pin(async move { Err(fail) }),
                Act::Hang => Box::pin(std::future::pending()),
            }
        }
    }

    impl Swappable for Instance {
        fn quiesce(&self) -> BoxFuture<'_, Result<(), StepError>> {
            self.act("quiesce", 0, ())
        }
        fn resume(&self) -> BoxFuture<'_, Result<(), StepError>> {
            self.act("resume", 1, ())
        }
        fn export_state(&self) -> BoxFuture<'_, Result<SeamState, StepError>> {
            let state = SeamState {
                version: 1,
                bytes: self.label.as_bytes().to_vec(),
            };
            self.act("export", 2, state)
        }
        fn import_state(&self, state: SeamState) -> BoxFuture<'_, Result<(), StepError>> {
            *lock(&self.imported) = Some(state);
            self.act("import", 3, ())
        }
        fn retire(&self) -> BoxFuture<'_, Result<(), StepError>> {
            self.act("retire", 4, ())
        }
    }

    impl Probe for Instance {
        fn label(&self) -> &str {
            &self.label
        }
        fn imported(&self) -> Option<SeamState> {
            lock(&self.imported).clone()
        }
    }

    /// Builds an instance labelled with the implementation name, scripted by
    /// the configuration payload: one byte per method, 0 ok, 1 fail, 2 hang.
    fn factory<S: Seam<Object = dyn Probe, Context = Log>>(label: &'static str) -> Factory<S> {
        Arc::new(move |log: &Log, config: &ConfigValue| {
            lock(log).push(format!("build {label}"));
            if config.version() == 9 {
                return Err(BuildError::Failed(Detail::new("refused")));
            }
            let mut script = ALL_OK;
            for (act, byte) in script.iter_mut().zip(config.payload()) {
                *act = Act::from_byte(*byte);
            }
            let instance: Arc<dyn Probe> = Arc::new(Instance {
                label: label.to_owned(),
                script,
                log: log.clone(),
                imported: Mutex::new(None),
            });
            Ok(instance)
        })
    }

    fn binding(name: &str, script: Script) -> Binding {
        let payload = script.iter().map(|act| *act as u8).collect();
        Binding {
            name: ImplName::new(name).unwrap(),
            config: ConfigValue::new(1, payload).unwrap(),
        }
    }

    struct Lab {
        registry: Registry,
        log: Log,
    }

    impl Lab {
        fn new() -> Lab {
            let mut builder = RegistryBuilder::new();
            for label in ["a", "b"] {
                let name = ImplName::new(label).unwrap();
                builder
                    .register::<ProbeSeam>(name.clone(), factory::<ProbeSeam>(label))
                    .unwrap();
                builder
                    .register::<DrainProbeSeam>(name, factory::<DrainProbeSeam>(label))
                    .unwrap();
            }
            Lab {
                registry: builder.build(),
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// A slot serving implementation `a` with `script`, and an empty log.
        fn slot<S: Seam<Object = dyn Probe, Context = Log>>(&self, script: Script) -> Slot<S> {
            let from = binding("a", script);
            let instance = self
                .registry
                .instantiate::<S>(&self.log, &from.name, &from.config)
                .unwrap();
            lock(&self.log).clear();
            Slot::new(from, instance)
        }

        fn log(&self) -> Vec<String> {
            lock(&self.log).clone()
        }
    }

    /// A deadline that completes once `fired` is set, counting its polls.
    struct Deadline {
        fired: Arc<AtomicBool>,
        polls: Arc<AtomicUsize>,
    }

    impl Deadline {
        fn new(fired: bool) -> Deadline {
            Deadline {
                fired: Arc::new(AtomicBool::new(fired)),
                polls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn future(&self) -> BoxFuture<'static, ()> {
            let fired = self.fired.clone();
            let polls = self.polls.clone();
            Box::pin(poll_fn(move |_| {
                polls.fetch_add(1, Ordering::SeqCst);
                if fired.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }))
        }

        fn fire(&self) {
            self.fired.store(true, Ordering::SeqCst);
        }

        fn polls(&self) -> usize {
            self.polls.load(Ordering::SeqCst)
        }
    }

    fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    fn run<T>(mut future: BoxFuture<'_, T>) -> T {
        match poll(future.as_mut()) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the future did not complete in one poll"),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn step_error(text: &str) -> StepError {
        StepError {
            detail: Detail::new(text),
        }
    }

    #[test]
    fn a_swap_runs_every_step_in_order_and_activates() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>(ALL_OK);
        let deadline = Deadline::new(false);
        let to = binding("b", ALL_OK);
        let result = run(slot.swap(&lab.registry, &lab.log, to.clone(), deadline.future()));
        assert_eq!(
            result,
            Ok(Swapped {
                generation: 1,
                retired: Ok(())
            })
        );
        assert_eq!(
            lab.log(),
            strings(&["build b", "a.quiesce", "a.export", "b.import", "a.retire"])
        );
        assert_eq!(slot.current().label(), "b");
        assert_eq!(
            slot.current().imported(),
            Some(SeamState {
                version: 1,
                bytes: b"a".to_vec()
            })
        );
        assert_eq!(slot.binding(), to);
        assert_eq!(slot.generation(), 1);

        // A completed swap leaves the slot free for the next one.
        let back = binding("a", ALL_OK);
        let result = run(slot.swap(&lab.registry, &lab.log, back.clone(), deadline.future()));
        assert_eq!(result.map(|s| s.generation), Ok(2));
        assert_eq!(slot.binding(), back);
    }

    #[test]
    fn a_failed_retire_of_the_old_instance_still_swaps() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>([Act::Ok, Act::Ok, Act::Ok, Act::Ok, Act::Fail]);
        let deadline = Deadline::new(false);
        let result = run(slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            deadline.future(),
        ));
        assert_eq!(
            result,
            Ok(Swapped {
                generation: 1,
                retired: Err(step_error("a.retire failed"))
            })
        );
        assert_eq!(slot.current().label(), "b");
    }

    #[test]
    fn a_prepare_failure_leaves_the_old_instance_untouched() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>(ALL_OK);
        let from = slot.binding();
        let deadline = Deadline::new(false);
        let missing = binding("c", ALL_OK);
        assert_eq!(
            run(slot.swap(&lab.registry, &lab.log, missing, deadline.future())),
            Err(SwapError::Prepare(BuildError::SeamUnavailable {
                kind: crate::Kind::new("probe").unwrap(),
                name: ImplName::new("c").unwrap()
            }))
        );
        let refused = Binding {
            name: ImplName::new("b").unwrap(),
            config: ConfigValue::defaults(9).unwrap(),
        };
        assert_eq!(
            run(slot.swap(&lab.registry, &lab.log, refused, deadline.future())),
            Err(SwapError::Prepare(BuildError::Failed(Detail::new(
                "refused"
            ))))
        );
        assert_eq!(lab.log(), strings(&["build b"]));
        assert_eq!(
            (slot.current().label(), slot.binding(), slot.generation()),
            ("a", from, 0)
        );
    }

    #[test]
    fn a_failed_quiesce_or_transfer_resumes_the_old_and_retires_the_new() {
        let cases = [
            (
                [Act::Fail, Act::Ok, Act::Ok, Act::Ok, Act::Ok],
                ALL_OK,
                SwapStep::Quiesce,
                "a.quiesce failed",
                &["build b", "a.quiesce", "a.resume", "b.retire"][..],
            ),
            (
                [Act::Ok, Act::Ok, Act::Fail, Act::Ok, Act::Ok],
                ALL_OK,
                SwapStep::Transfer,
                "a.export failed",
                &["build b", "a.quiesce", "a.export", "a.resume", "b.retire"][..],
            ),
            (
                ALL_OK,
                [Act::Ok, Act::Ok, Act::Ok, Act::Fail, Act::Ok],
                SwapStep::Transfer,
                "b.import failed",
                &[
                    "build b",
                    "a.quiesce",
                    "a.export",
                    "b.import",
                    "a.resume",
                    "b.retire",
                ][..],
            ),
        ];
        for (old, new, step, cause, log) in cases {
            let lab = Lab::new();
            let slot = lab.slot::<ProbeSeam>(old);
            let deadline = Deadline::new(false);
            assert_eq!(
                run(slot.swap(
                    &lab.registry,
                    &lab.log,
                    binding("b", new),
                    deadline.future()
                )),
                Err(SwapError::Failed {
                    step,
                    cause: step_error(cause),
                    resumed: Ok(())
                })
            );
            assert_eq!(lab.log(), strings(log));
            assert_eq!((slot.current().label(), slot.generation()), ("a", 0));
        }
    }

    #[test]
    fn a_failed_resume_is_reported() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>([Act::Fail, Act::Fail, Act::Ok, Act::Ok, Act::Ok]);
        let deadline = Deadline::new(false);
        assert_eq!(
            run(slot.swap(
                &lab.registry,
                &lab.log,
                binding("b", ALL_OK),
                deadline.future()
            )),
            Err(SwapError::Failed {
                step: SwapStep::Quiesce,
                cause: step_error("a.quiesce failed"),
                resumed: Err(step_error("a.resume failed"))
            })
        );
    }

    #[test]
    fn a_completed_deadline_stops_before_prepare() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>(ALL_OK);
        let deadline = Deadline::new(true);
        assert_eq!(
            run(slot.swap(
                &lab.registry,
                &lab.log,
                binding("b", ALL_OK),
                deadline.future()
            )),
            Err(SwapError::Deadline {
                step: SwapStep::Prepare,
                resumed: Ok(())
            })
        );
        assert_eq!(lab.log(), Vec::<String>::new());
        assert_eq!(deadline.polls(), 1);
        // The slot is not poisoned: a later swap runs.
        let deadline = Deadline::new(false);
        let result = run(slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            deadline.future(),
        ));
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn a_deadline_during_a_step_drops_it_and_cleans_up() {
        let cases = [
            (
                [Act::Hang, Act::Ok, Act::Ok, Act::Ok, Act::Ok],
                ALL_OK,
                SwapStep::Quiesce,
                &["build b", "a.quiesce", "a.resume", "b.retire"][..],
            ),
            (
                [Act::Ok, Act::Ok, Act::Hang, Act::Ok, Act::Ok],
                ALL_OK,
                SwapStep::Transfer,
                &["build b", "a.quiesce", "a.export", "a.resume", "b.retire"][..],
            ),
            (
                ALL_OK,
                [Act::Ok, Act::Ok, Act::Ok, Act::Hang, Act::Ok],
                SwapStep::Transfer,
                &[
                    "build b",
                    "a.quiesce",
                    "a.export",
                    "b.import",
                    "a.resume",
                    "b.retire",
                ][..],
            ),
        ];
        for (old, new, step, log) in cases {
            let lab = Lab::new();
            let slot = lab.slot::<ProbeSeam>(old);
            let deadline = Deadline::new(false);
            let mut swap = slot.swap(
                &lab.registry,
                &lab.log,
                binding("b", new),
                deadline.future(),
            );
            assert!(poll(swap.as_mut()).is_pending());
            assert!(poll(swap.as_mut()).is_pending());
            // Still serving the old instance while the step hangs.
            assert_eq!((slot.current().label(), slot.generation()), ("a", 0));
            let polls = deadline.polls();
            deadline.fire();
            assert_eq!(
                poll(swap.as_mut()),
                Poll::Ready(Err(SwapError::Deadline {
                    step,
                    resumed: Ok(())
                }))
            );
            // The deadline was polled first, and not again after it completed.
            assert_eq!(deadline.polls(), polls + 1);
            assert_eq!(lab.log(), strings(log));
            drop(swap);
            assert_eq!((slot.current().label(), slot.generation()), ("a", 0));
        }
    }

    #[test]
    fn the_deadline_is_ignored_after_activation() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>([Act::Ok, Act::Ok, Act::Ok, Act::Ok, Act::Hang]);
        let deadline = Deadline::new(false);
        let mut swap = slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            deadline.future(),
        );
        assert!(poll(swap.as_mut()).is_pending());
        // Activated: the old instance's retire is what hangs.
        assert_eq!((slot.current().label(), slot.generation()), ("b", 1));
        let polls = deadline.polls();
        deadline.fire();
        assert!(poll(swap.as_mut()).is_pending());
        assert_eq!(deadline.polls(), polls);
        // Dropped while running, even after activation, it poisons the slot.
        drop(swap);
        assert_eq!(
            run(slot.swap(
                &lab.registry,
                &lab.log,
                binding("a", ALL_OK),
                deadline.future()
            )),
            Err(SwapError::Poisoned)
        );
        assert_eq!((slot.current().label(), slot.generation()), ("b", 1));
    }

    #[test]
    fn one_swap_runs_at_a_time_and_a_dropped_one_poisons_the_slot() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>([Act::Hang, Act::Ok, Act::Ok, Act::Ok, Act::Ok]);
        let first_deadline = Deadline::new(false);
        let mut first = slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            first_deadline.future(),
        );
        // Built but not polled: nothing has started.
        let second_deadline = Deadline::new(true);
        let second = slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            second_deadline.future(),
        );
        assert!(poll(first.as_mut()).is_pending());
        assert_eq!(run(second), Err(SwapError::Busy));
        assert_eq!(second_deadline.polls(), 0);

        drop(first);
        let third_deadline = Deadline::new(false);
        assert_eq!(
            run(slot.swap(
                &lab.registry,
                &lab.log,
                binding("b", ALL_OK),
                third_deadline.future()
            )),
            Err(SwapError::Poisoned)
        );
        assert_eq!(third_deadline.polls(), 0);
        assert_eq!(lab.log(), strings(&["build b", "a.quiesce"]));
        assert_eq!((slot.current().label(), slot.generation()), ("a", 0));
    }

    #[test]
    fn a_future_never_polled_or_completed_does_not_poison() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>(ALL_OK);
        let deadline = Deadline::new(false);
        drop(slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            deadline.future(),
        ));
        assert_eq!(lab.log(), Vec::<String>::new());
        let mut done = slot.swap(
            &lab.registry,
            &lab.log,
            binding("b", ALL_OK),
            deadline.future(),
        );
        assert!(poll(done.as_mut()).is_ready());
        drop(done);
        let result = run(slot.swap(
            &lab.registry,
            &lab.log,
            binding("a", ALL_OK),
            deadline.future(),
        ));
        assert_eq!(result.map(|s| s.generation), Ok(2));
    }

    #[test]
    fn a_seam_that_is_not_live_does_not_swap() {
        let lab = Lab::new();
        let slot = lab.slot::<DrainProbeSeam>(ALL_OK);
        let deadline = Deadline::new(false);
        assert_eq!(
            run(slot.swap(
                &lab.registry,
                &lab.log,
                binding("b", ALL_OK),
                deadline.future()
            )),
            Err(SwapError::NotLive)
        );
        assert_eq!(deadline.polls(), 0);
        assert_eq!(lab.log(), Vec::<String>::new());
    }

    #[test]
    fn debug_shows_the_binding_without_its_payload() {
        let lab = Lab::new();
        let slot = lab.slot::<ProbeSeam>([Act::Ok, Act::Ok, Act::Ok, Act::Ok, Act::Fail]);
        let text = format!("{slot:?}");
        assert!(
            text.contains("ConfigValue { version: 1, len: 5 }"),
            "{text}"
        );
        assert!(text.contains("generation: 0"), "{text}");
        assert!(text.contains("kind: \"probe\""), "{text}");
    }
}
