// SPDX-License-Identifier: MIT OR Apache-2.0

//! Audited wrappers for the small set of state that outlives a Stone process.

use std::cell::RefCell;
use std::ops::Deref;
#[cfg(not(target_os = "hermit"))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_os = "hermit"))]
use std::sync::Once;

use crate::vm_supervisor::ControlScope;

#[cfg(not(target_os = "hermit"))]
pub(crate) struct VmAtomicU64(AtomicU64);

#[cfg(not(target_os = "hermit"))]
impl VmAtomicU64 {
    pub(crate) const fn new(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }

    pub(crate) fn fetch_add_relaxed(&self, value: u64) -> u64 {
        self.0.fetch_add(value, Ordering::Relaxed)
    }
}

/// Marker for immutable values that can safely outlive every Stone process.
///
/// # Safety
///
/// The value must not contain process-domain pointers, mutable attempt state,
/// external resource ownership, or process-dependent lazy initialization.
pub(crate) unsafe trait FreezeSafe: Sync + 'static {}

pub(crate) struct VmFrozen<T: FreezeSafe>(T);

impl<T: FreezeSafe> VmFrozen<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T: FreezeSafe> Deref for VmFrozen<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A process-wide once flag that retains no closure or process-owned value.
#[cfg(not(target_os = "hermit"))]
pub(crate) struct VmOnce(Once);

#[cfg(not(target_os = "hermit"))]
impl VmOnce {
    pub(crate) const fn new() -> Self {
        Self(Once::new())
    }

    pub(crate) fn call_once(&self, operation: impl FnOnce()) {
        self.0.call_once(operation);
    }
}

/// Marker for state owned by one Stone process thread.
///
/// # Safety
///
/// The value must be safe to drop when its process thread exits. It must never
/// publish a process-domain reference into VM-global state.
pub(crate) unsafe trait ProcessLocalState: 'static {}

pub(crate) struct ProcessTls<T: ProcessLocalState>(RefCell<T>);

impl<T: ProcessLocalState> ProcessTls<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(RefCell::new(value))
    }

    pub(crate) fn with<R>(&self, operation: impl FnOnce(&T) -> R) -> R {
        operation(&self.0.borrow())
    }

    pub(crate) fn with_mut<R>(&self, operation: impl FnOnce(&mut T) -> R) -> R {
        operation(&mut self.0.borrow_mut())
    }

    pub(crate) fn replace(&self, value: T) -> T {
        self.0.replace(value)
    }
}

/// Initialize approved VM-lifetime caches while allocations belong to the
/// control domain.
///
/// There are currently no heap-owning VM-global caches. Keep this explicit
/// hook so adding one requires a `ControlScope` and an allow-list review.
pub(crate) fn prewarm_vm_globals(_control: &ControlScope) {}

#[cfg(test)]
mod tests {
    #[test]
    fn patched_nu_quoting_cache_can_be_created_and_dropped_per_thread() {
        for _ in 0..8 {
            std::thread::spawn(|| {
                assert!(nu_utils::needs_quoting("two words"));
                assert!(!nu_utils::needs_quoting("one_word"));
            })
            .join()
            .expect("Nu quoting worker should exit cleanly");
        }
    }
}
