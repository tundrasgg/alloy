//! Whether the calling thread is inside a hook: an observer or a middlebox of
//! any world (ARFC 0005 clause 3.2.9).
//!
//! A world call made from inside a hook changes nothing, on the hook's own
//! world and on every other world alike. The mark is per thread and counts
//! nesting, so a hook of one world that runs inside a hook of another leaves
//! the thread marked until both have returned. This is the only state the
//! simulator keeps outside a world: runs share no other state, and the mark
//! changes no record, no draw, and no result of a call made outside a hook.

use std::cell::Cell;
use std::marker::PhantomData;

thread_local! {
    /// Hooks the calling thread is running, nested.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// True when the calling thread is running an observer or a middlebox of any
/// world.
pub(crate) fn inside() -> bool {
    DEPTH.with(|depth| depth.get() > 0)
}

/// Marks the calling thread as running a hook until the guard drops.
pub(crate) fn enter() -> Inside {
    DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    Inside(PhantomData)
}

/// Clears one level of the mark of [`enter`] when dropped. It stays on the
/// thread that made it.
pub(crate) struct Inside(PhantomData<*const ()>);

impl Drop for Inside {
    fn drop(&mut self) {
        DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_nests_and_clears() {
        assert!(!inside());
        let outer = enter();
        let inner = enter();
        assert!(inside());
        drop(inner);
        assert!(inside());
        drop(outer);
        assert!(!inside());
    }
}
