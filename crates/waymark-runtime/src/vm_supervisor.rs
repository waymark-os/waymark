// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ownership and lifecycle boundary for a reusable Waymark LibOS instance.
//!
//! This is the host-native first slice. It deliberately runs one fresh root
//! process at a time and uses the same state machine that the Hermit provider
//! will use. Hermit builds additionally place the process thread in an isolated
//! allocation domain and reclaim that domain after the thread is joined.

use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::thread;

use serde_json::{json, Value as JsonValue};

use crate::agent::AgentModelGateway;
use crate::gateway_runtime;
use crate::global_state::prewarm_vm_globals;
#[cfg(not(target_os = "hermit"))]
use crate::global_state::VmAtomicU64;
use crate::{StoneGuest, TaskScopeSnapshot};

const STONE_PROCESS_STACK_SIZE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmLifecycleState {
    Ready,
    Leased,
    Draining,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessId {
    pub slot: u32,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceId {
    pub slot: u32,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCleanlinessReport {
    pub process: ProcessId,
    pub profile: &'static str,
    pub reusable: bool,
    pub live_processes: usize,
    pub live_threads: usize,
    pub live_resources: usize,
    pub live_domains: usize,
    pub process_domain_bytes_before_release: u64,
    pub process_domain_allocations_before_release: u64,
    pub completed_dispatches: u64,
    pub exercised: Vec<String>,
    pub gateway_release: Option<JsonValue>,
    pub reasons: Vec<String>,
}

impl VmCleanlinessReport {
    pub fn to_json(&self) -> JsonValue {
        json!({
            "profile": self.profile,
            "reusable": self.reusable,
            "process": {
                "slot": self.process.slot,
                "generation": self.process.generation,
            },
            "live_processes": self.live_processes,
            "live_threads": self.live_threads,
            "live_resources": self.live_resources,
            "live_domains": self.live_domains,
            "process_domain_bytes_before_release": self.process_domain_bytes_before_release,
            "process_domain_allocations_before_release": self.process_domain_allocations_before_release,
            "completed_dispatches": self.completed_dispatches,
            "exercised": self.exercised,
            "gateway_release": self.gateway_release,
            "reasons": self.reasons,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmDispatchResult {
    pub process: ProcessId,
    pub response: JsonValue,
    pub cleanliness: VmCleanlinessReport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmDispatchError {
    pub code: &'static str,
    pub message: String,
    pub cleanliness: Option<VmCleanlinessReport>,
}

impl fmt::Display for VmDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VmDispatchError {}

/// Long-lived VM control state. No `StoneGuest` is stored here: every dispatch
/// constructs a fresh process-local runtime on its worker thread.
#[derive(Debug)]
pub struct VmSupervisor {
    start_dir: PathBuf,
    state: VmLifecycleState,
    next_generation: u64,
    completed_dispatches: u64,
    poison_reason: Option<String>,
}

pub(crate) struct ControlScope {
    _private: (),
}

impl ControlScope {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl VmSupervisor {
    pub fn new(start_dir: PathBuf) -> Self {
        let control = ControlScope::new();
        prewarm_vm_globals(&control);
        Self {
            start_dir,
            state: VmLifecycleState::Ready,
            next_generation: 1,
            completed_dispatches: 0,
            poison_reason: None,
        }
    }

    pub fn state(&self) -> VmLifecycleState {
        self.state
    }

    pub fn poison_reason(&self) -> Option<&str> {
        self.poison_reason.as_deref()
    }

    pub(crate) fn start_dir(&self) -> &std::path::Path {
        &self.start_dir
    }

    /// Runs one unrelated root task in a fresh Stone process.
    ///
    /// A task-level failure is an ordinary JSON response and does not poison the
    /// VM. A failed spawn/join, live process resource, exhausted allocation
    /// domain, failed domain release, or invalid promoted result does poison it.
    pub fn dispatch_task(&mut self, task: JsonValue) -> Result<VmDispatchResult, VmDispatchError> {
        let probes = ProcessProbes::from_task(&task);
        self.dispatch_operation(probes, false, move |guest| {
            guest.task_response_from_value(task)
        })
    }

    /// Runs one root task with a process-scoped model and host-capability
    /// gateway. The borrowed gateway cannot escape the scoped worker thread.
    pub fn dispatch_task_with_model_gateway<G>(
        &mut self,
        task: JsonValue,
        gateway: &mut G,
    ) -> Result<VmDispatchResult, VmDispatchError>
    where
        G: AgentModelGateway + Send,
    {
        let probes = ProcessProbes::from_task(&task);
        self.dispatch_operation(probes, false, move |guest| {
            guest.task_response_from_value_with_model_gateway(task, gateway)
        })
    }

    pub(crate) fn dispatch_gateway_attempt_lease(
        &mut self,
        config: gateway_runtime::GatewayRuntimeConfig,
        touch_nu_quoting_tls: bool,
    ) -> Result<VmDispatchResult, VmDispatchError> {
        self.dispatch_operation(
            ProcessProbes {
                nu_quoting_tls: touch_nu_quoting_tls,
            },
            true,
            move |guest| {
                gateway_runtime::set_config(Some(config));
                guest.task_response_from_gateway_attempt()
            },
        )
    }

    fn dispatch_operation<F>(
        &mut self,
        probes: ProcessProbes,
        release_gateway_lease: bool,
        operation: F,
    ) -> Result<VmDispatchResult, VmDispatchError>
    where
        F: FnOnce(&mut StoneGuest) -> JsonValue + Send,
    {
        if self.state != VmLifecycleState::Ready {
            return Err(VmDispatchError {
                code: "vm_not_ready",
                message: match &self.poison_reason {
                    Some(reason) => format!("VM is {:?}: {reason}", self.state),
                    None => format!("VM is {:?}", self.state),
                },
                cleanliness: None,
            });
        }

        let process = ProcessId {
            slot: 0,
            generation: self.next_generation,
        };
        self.next_generation = self.next_generation.saturating_add(1);
        self.state = VmLifecycleState::Leased;

        let domain = match ProcessDomainOwner::create() {
            Ok(domain) => domain,
            Err(err) => {
                return Err(self.poison("process_domain_create_failed", err));
            }
        };
        let domain_id = domain.id();
        let start_dir = self.start_dir.clone();
        let worker_run = thread::scope(|scope| {
            let worker = thread::Builder::new()
                .name(format!("stone-root-{}", process.generation))
                .stack_size(STONE_PROCESS_STACK_SIZE)
                .spawn_scoped(scope, move || {
                    run_fresh_root_process(
                        domain_id,
                        start_dir,
                        probes,
                        release_gateway_lease,
                        operation,
                    )
                });
            match worker {
                Ok(worker) => {
                    self.state = VmLifecycleState::Draining;
                    match worker.join() {
                        Ok(exit) => WorkerRun::Exited(exit),
                        Err(_) => WorkerRun::Panicked,
                    }
                }
                Err(err) => WorkerRun::SpawnFailed(err.to_string()),
            }
        });

        let worker_exit = match worker_run {
            WorkerRun::Exited(exit) => Some(exit),
            WorkerRun::SpawnFailed(err) => {
                let release_error = domain.release().err();
                let mut message = format!("failed to spawn Stone process: {err}");
                if let Some(release_error) = release_error {
                    message.push_str(&format!("; {release_error}"));
                }
                return Err(self.poison("process_spawn_failed", message));
            }
            WorkerRun::Panicked => None,
        };

        let mut reasons = Vec::new();
        let mut live_resources = 0;
        let mut exercised = Vec::new();
        let mut process_domain_bytes = 0;
        let mut process_domain_allocations = 0;
        let mut gateway_release = None;
        let promoted = match worker_exit {
            Some(exit) => {
                live_resources = exit.live_resources;
                exercised = exit.exercised;
                process_domain_bytes = exit.process_domain_bytes;
                process_domain_allocations = exit.process_domain_allocations;
                gateway_release = exit.gateway_release;
                if let Some(reason) = exit.cleanup_error {
                    reasons.push(reason);
                }
                Some(exit.response)
            }
            None => {
                reasons.push("Stone process thread panicked".to_owned());
                None
            }
        };

        if domain.take_exhausted() {
            reasons.push("Stone process allocation domain exhausted".to_owned());
        }
        let live_domains = match domain.release() {
            Ok(()) => 0,
            Err(err) => {
                reasons.push(err);
                1
            }
        };

        self.completed_dispatches = self.completed_dispatches.saturating_add(1);
        let response = match &promoted {
            Some(response) => match serde_json::from_slice(response.as_ref()) {
                Ok(response) => Some(response),
                Err(err) => {
                    reasons.push(format!("promoted process result is invalid JSON: {err}"));
                    None
                }
            },
            None => None,
        };
        drop(promoted);

        let reusable = reasons.is_empty() && live_resources == 0;
        let report = VmCleanlinessReport {
            process,
            profile: if cfg!(target_os = "hermit") {
                "hermit-allocation-domain-v0"
            } else {
                "host-native-fresh-root-v0"
            },
            reusable,
            live_processes: 0,
            live_threads: 0,
            live_resources,
            live_domains,
            process_domain_bytes_before_release: process_domain_bytes,
            process_domain_allocations_before_release: process_domain_allocations,
            completed_dispatches: self.completed_dispatches,
            exercised,
            gateway_release,
            reasons,
        };

        if reusable {
            self.state = VmLifecycleState::Ready;
            Ok(VmDispatchResult {
                process,
                response: response.expect("clean dispatch has a promoted response"),
                cleanliness: report,
            })
        } else {
            let message = report.reasons.join("; ");
            self.poison_reason = Some(message.clone());
            self.state = VmLifecycleState::Poisoned;
            Err(VmDispatchError {
                code: "vm_cleanup_failed",
                message,
                cleanliness: Some(report),
            })
        }
    }

    fn poison(&mut self, code: &'static str, message: String) -> VmDispatchError {
        self.poison_reason = Some(message.clone());
        self.state = VmLifecycleState::Poisoned;
        VmDispatchError {
            code,
            message,
            cleanliness: None,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ProcessProbes {
    nu_quoting_tls: bool,
}

impl ProcessProbes {
    fn from_task(task: &JsonValue) -> Self {
        Self {
            nu_quoting_tls: task
                .get("diagnostics")
                .and_then(|diagnostics| diagnostics.get("touch_nu_quoting_tls"))
                .and_then(JsonValue::as_bool)
                .unwrap_or(false),
        }
    }
}

enum WorkerRun {
    Exited(WorkerExit),
    SpawnFailed(String),
    Panicked,
}

/// A lifetime brand for one process-domain execution. Process-local references
/// cannot be returned from `with_process_scope` because its closure is
/// higher-ranked over this lifetime.
pub struct ProcessScope<'process> {
    _brand: PhantomData<&'process mut ()>,
}

/// Marker for values whose destruction has no effects beyond releasing memory.
///
/// # Safety
///
/// Implementations must not own external resources, locks, channels, process
/// handles, or values whose `Drop` is required for correctness. This trait is
/// sealed; only audited runtime representations may implement it.
pub unsafe trait BulkResetSafe: bulk_reset_sealed::Sealed {}

mod bulk_reset_sealed {
    pub trait Sealed {}
}

macro_rules! bulk_reset_scalar {
    ($($type:ty),* $(,)?) => {$ (
        impl bulk_reset_sealed::Sealed for $type {}
        // SAFETY: scalar values have no destructor or external ownership.
        unsafe impl BulkResetSafe for $type {}
    )* };
}

bulk_reset_scalar!(bool, i64, u64, f64);

impl bulk_reset_sealed::Sealed for String {}
// SAFETY: `String` owns only its memory buffer.
unsafe impl BulkResetSafe for String {}

impl<T: BulkResetSafe> bulk_reset_sealed::Sealed for Vec<T> {}
// SAFETY: the vector owns only its buffer and audited bulk-reset-safe elements.
unsafe impl<T: BulkResetSafe> BulkResetSafe for Vec<T> {}

pub struct ProcessBox<'process, T: BulkResetSafe> {
    value: Box<T>,
    _brand: PhantomData<&'process mut ()>,
}

impl<'process> ProcessScope<'process> {
    pub fn alloc<T: BulkResetSafe>(&self, value: T) -> ProcessBox<'process, T> {
        ProcessBox {
            value: Box::new(value),
            _brand: PhantomData,
        }
    }
}

impl<T: BulkResetSafe> std::ops::Deref for ProcessBox<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// A value copied into the supervisor/control allocation domain after process
/// execution and before the process domain is released.
struct Promoted<T>(T);

impl<T> Promoted<T> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

struct WorkerExit {
    response: Promoted<Vec<u8>>,
    live_resources: usize,
    cleanup_error: Option<String>,
    exercised: Vec<String>,
    process_domain_bytes: u64,
    process_domain_allocations: u64,
    gateway_release: Option<JsonValue>,
}

struct ProcessExit {
    response: Vec<u8>,
    scope: TaskScopeSnapshot,
    cleanup_error: Option<String>,
    exercised: Vec<String>,
    process_domain_bytes: u64,
    process_domain_allocations: u64,
    gateway_release: Option<JsonValue>,
}

fn run_fresh_root_process<F>(
    domain: ProcessDomainId,
    start_dir: PathBuf,
    probes: ProcessProbes,
    release_gateway_lease: bool,
    operation: F,
) -> WorkerExit
where
    F: FnOnce(&mut StoneGuest) -> JsonValue,
{
    let process_exit = with_process_scope(domain, |_scope| {
        gateway_runtime::reset_process_state();
        let mut exercised = Vec::new();
        if probes.nu_quoting_tls {
            assert!(nu_utils::needs_quoting("two words"));
            exercised.push("nu_quoting_tls".to_owned());
        }
        let (response, scope, mut cleanup_error) = match StoneGuest::new(start_dir) {
            Ok(mut guest) => {
                let response = operation(&mut guest);
                let cleanup_error = guest.reset_task_state().err().map(|err| err.to_string());
                let scope = guest.task_scope_snapshot();
                (response, scope, cleanup_error)
            }
            Err(err) => (
                json!({
                    "ok": false,
                    "error": {
                        "kind": "runtime",
                        "code": "stone_process_init_failed",
                        "message": err.to_string(),
                    }
                }),
                TaskScopeSnapshot::default(),
                None,
            ),
        };
        let gateway_release = if release_gateway_lease {
            match gateway_runtime::release_provider_lease() {
                Ok(evidence) => {
                    if !evidence.clean {
                        let reason = format!(
                            "Gateway provider lease cleanup failed: {}",
                            evidence.reasons.join("; ")
                        );
                        cleanup_error = Some(match cleanup_error {
                            Some(existing) => format!("{existing}; {reason}"),
                            None => reason,
                        });
                    }
                    Some(evidence.to_json())
                }
                Err(err) => {
                    let reason = format!("Gateway provider lease release failed: {err}");
                    cleanup_error = Some(match cleanup_error {
                        Some(existing) => format!("{existing}; {reason}"),
                        None => reason,
                    });
                    None
                }
            }
        } else {
            None
        };
        gateway_runtime::reset_process_state();
        let response = serde_json::to_vec(&response).unwrap_or_else(|err| {
            serde_json::to_vec(&json!({
                "ok": false,
                "error": {
                    "kind": "runtime",
                    "code": "stone_process_result_encode_failed",
                    "message": err.to_string(),
                }
            }))
            .expect("static process encode error response is valid JSON")
        });
        let (process_domain_bytes, process_domain_allocations) = current_process_domain_usage();
        ProcessExit {
            response,
            scope,
            cleanup_error,
            exercised,
            process_domain_bytes,
            process_domain_allocations,
            gateway_release,
        }
    });

    // `with_process_scope` restored the control domain. Clone every value that
    // must survive the join, then drop the originals while their domain is
    // still alive. The supervisor releases the domain only after joining us.
    let response = Promoted(process_exit.response.clone());
    let cleanup_error = process_exit.cleanup_error.clone();
    let exercised = process_exit.exercised.clone();
    let process_domain_bytes = process_exit.process_domain_bytes;
    let process_domain_allocations = process_exit.process_domain_allocations;
    let gateway_release = process_exit.gateway_release.clone();
    let live_resources = process_exit.scope.live.len();
    drop(process_exit);

    WorkerExit {
        response,
        live_resources,
        cleanup_error,
        exercised,
        process_domain_bytes,
        process_domain_allocations,
        gateway_release,
    }
}

#[cfg(target_os = "hermit")]
fn current_process_domain_usage() -> (u64, u64) {
    let mut stats = hermit_abi::mem_stats::default();
    // SAFETY: the kernel writes one plain `repr(C)` statistics value and does
    // not retain the pointer.
    let status = unsafe { hermit_abi::mem_stats(&mut stats) };
    if status < 0 {
        (0, 0)
    } else {
        (
            stats.alloc_domain_current_bytes,
            stats.alloc_domain_current_count,
        )
    }
}

#[cfg(not(target_os = "hermit"))]
fn current_process_domain_usage() -> (u64, u64) {
    (0, 0)
}

fn with_process_scope<T, F>(domain: ProcessDomainId, operation: F) -> T
where
    F: for<'process> FnOnce(ProcessScope<'process>) -> T,
{
    let _guard = ProcessDomainGuard::enter(domain);
    operation(ProcessScope {
        _brand: PhantomData,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessDomainId(u64);

struct ProcessDomainOwner {
    id: ProcessDomainId,
}

impl ProcessDomainOwner {
    fn create() -> Result<Self, String> {
        let id = create_process_domain();
        if cfg!(target_os = "hermit") && id == 0 {
            return Err(
                "Hermit allocation domains are unavailable; build the guest with alloc-domains"
                    .to_owned(),
            );
        }
        Ok(Self {
            id: ProcessDomainId(id),
        })
    }

    fn id(&self) -> ProcessDomainId {
        self.id
    }

    fn take_exhausted(&self) -> bool {
        take_process_domain_exhausted(self.id.0)
    }

    fn release(self) -> Result<(), String> {
        release_process_domain(self.id.0)
    }
}

struct ProcessDomainGuard {
    previous: u64,
}

impl ProcessDomainGuard {
    fn enter(domain: ProcessDomainId) -> Self {
        Self {
            previous: set_process_domain(domain.0),
        }
    }
}

impl Drop for ProcessDomainGuard {
    fn drop(&mut self) {
        set_process_domain(self.previous);
    }
}

#[cfg(target_os = "hermit")]
fn create_process_domain() -> u64 {
    // SAFETY: this creates a new kernel-owned allocation domain and returns its
    // opaque identifier. No memory is exposed through the call.
    unsafe { hermit_abi::alloc_domain_create(hermit_abi::ALLOC_DOMAIN_CREATE_ISOLATED) }
}

#[cfg(not(target_os = "hermit"))]
fn create_process_domain() -> u64 {
    static NEXT_HOST_DOMAIN: VmAtomicU64 = VmAtomicU64::new(1);
    NEXT_HOST_DOMAIN.fetch_add_relaxed(1)
}

#[cfg(target_os = "hermit")]
fn set_process_domain(domain: u64) -> u64 {
    // SAFETY: `domain` is an opaque id created by the Hermit kernel or the
    // previous id returned by this syscall. Invalid ids are rejected by Hermit.
    unsafe { hermit_abi::alloc_domain_set(domain) }
}

#[cfg(not(target_os = "hermit"))]
fn set_process_domain(_domain: u64) -> u64 {
    0
}

#[cfg(target_os = "hermit")]
fn take_process_domain_exhausted(domain: u64) -> bool {
    // SAFETY: this only reads and clears the kernel exhaustion flag for an
    // owned opaque domain id.
    unsafe { hermit_abi::alloc_domain_take_exhausted(domain) != 0 }
}

#[cfg(not(target_os = "hermit"))]
fn take_process_domain_exhausted(_domain: u64) -> bool {
    false
}

#[cfg(target_os = "hermit")]
fn release_process_domain(domain: u64) -> Result<(), String> {
    // SAFETY: the owner is non-cloneable and calls this only after joining the
    // sole process thread and promoting its result.
    let status = unsafe { hermit_abi::alloc_domain_release(domain) };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "failed to release Hermit allocation domain {domain:#x}: status {status}"
        ))
    }
}

#[cfg(not(target_os = "hermit"))]
fn release_process_domain(_domain: u64) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{VmLifecycleState, VmSupervisor};

    #[test]
    fn reusable_supervisor_runs_fresh_root_processes() {
        let root = test_root("fresh-roots");
        fs::create_dir_all(&root).expect("create root");
        let mut supervisor = VmSupervisor::new(root.clone());

        let first = supervisor
            .dispatch_task(stone_task("first", "secret = 41\nemit(secret + 1)"))
            .expect("first dispatch");
        assert_eq!(first.response["ok"], json!(true));
        assert_eq!(first.response["value"], json!(42));
        assert!(first.cleanliness.reusable);
        assert_eq!(supervisor.state(), VmLifecycleState::Ready);

        let second = supervisor
            .dispatch_task(stone_task("second", "emit(secret)"))
            .expect("second dispatch is a clean task-level failure");
        assert_eq!(second.response["ok"], json!(false));
        assert_eq!(second.process.slot, first.process.slot);
        assert!(second.process.generation > first.process.generation);
        assert_eq!(second.cleanliness.completed_dispatches, 2);
        assert!(second.cleanliness.reusable);
        assert_eq!(supervisor.state(), VmLifecycleState::Ready);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn process_box_is_bound_to_process_scope() {
        let value = super::with_process_scope(super::ProcessDomainId(1), |scope| {
            let value = scope.alloc(String::from("process-local"));
            value.len()
        });
        assert_eq!(value, 13);
    }

    fn stone_task(id: &str, source: &str) -> serde_json::Value {
        json!({
            "version": 0,
            "id": id,
            "runtime": { "frontend": "stone" },
            "script": { "source": source },
        })
    }

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("waymark-vm-supervisor-{name}-{nonce}"))
    }
}
