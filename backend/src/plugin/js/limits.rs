//! Host-owned JS budgets, allocator ownership, deadlines and cancellation.
//!
//! Runtime field order stops and joins the watchdog before V8 disposal.

use crate::core::app::error::{Result, TingError};
use deno_core::{JsRuntime, v8};
use std::ffi::c_void;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(super) const TIMED_OUT: u8 = 1;
const MEMORY_EXCEEDED: u8 = 2;
pub(super) const CANCELLED: u8 = 3;

#[derive(Clone)]
pub(super) struct JsBudget(Arc<BudgetState>);

struct BudgetState {
    reason: AtomicU8,
    external_bytes: AtomicUsize,
    external_limit: usize,
    isolate: Mutex<Option<v8::IsolateHandle>>,
}

impl JsBudget {
    pub fn new(memory_limit: usize) -> Result<Self> {
        if memory_limit < 16 * 1024 * 1024 {
            return Err(TingError::InvalidRequest(
                "JS memory budget must be at least 16 MiB".into(),
            ));
        }
        Ok(Self(Arc::new(BudgetState {
            reason: AtomicU8::new(0),
            external_bytes: AtomicUsize::new(0),
            // Reserve three quarters for V8's heap and one quarter for backing
            // stores. Heap limits alone do not constrain ArrayBuffer memory.
            external_limit: memory_limit / 4,
            isolate: Mutex::new(None),
        })))
    }

    pub fn create_params(&self, memory_limit: usize) -> v8::CreateParams {
        // SAFETY: the allocator owns exactly one Arc reference. V8 calls
        // allocator_drop once, after its last backing store has been freed.
        let allocator =
            unsafe { v8::new_rust_allocator(Arc::into_raw(self.0.clone()), &ALLOCATOR) };
        v8::CreateParams::default()
            .heap_limits(0, memory_limit - self.0.external_limit)
            .array_buffer_allocator(allocator)
    }

    pub fn attach(&self, runtime: &mut JsRuntime) {
        let handle = runtime.v8_isolate().thread_safe_handle();
        *self.0.isolate.lock().unwrap() = Some(handle);
        let budget = self.clone();
        runtime.add_near_heap_limit_callback(move |current_limit, _| {
            budget.terminate(MEMORY_EXCEEDED);
            // V8 needs bounded emergency headroom to unwind termination rather
            // than invoking its fatal OOM handler. The instance is never reused.
            current_limit.saturating_add(16 * 1024 * 1024)
        });
    }

    pub fn copy_chunk<'a>(
        &self,
        scope: &mut v8::HandleScope<'a>,
        bytes: &[u8],
    ) -> anyhow::Result<v8::Local<'a, v8::Uint8Array>> {
        // Reserve before constructing a V8 backing store: ArrayBuffer::new is
        // infallible in this V8 binding and cannot report allocator rejection.
        let pointer = unsafe { allocate_uninitialized(&self.0, bytes.len()) };
        if pointer.is_null() {
            self.terminate(MEMORY_EXCEEDED);
            self.check()?;
            unreachable!();
        }
        // SAFETY: allocation has bytes.len() bytes. The backing-store deleter
        // owns the allocation and exactly one Arc, even if V8 creation fails.
        let backing = unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            v8::ArrayBuffer::new_backing_store_from_ptr(
                pointer,
                bytes.len(),
                drop_chunk,
                Arc::into_raw(self.0.clone()).cast_mut().cast::<c_void>(),
            )
        }
        .make_shared();
        let buffer = v8::ArrayBuffer::with_backing_store(scope, &backing);
        v8::Uint8Array::new(scope, buffer, 0, bytes.len())
            .ok_or_else(|| anyhow::anyhow!("Failed to create chunk view"))
    }

    pub(super) fn terminate(&self, reason: u8) {
        self.0
            .reason
            .compare_exchange(0, reason, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        if let Some(handle) = self.0.isolate.lock().unwrap().as_ref() {
            handle.terminate_execution();
        }
    }

    pub fn check(&self) -> Result<()> {
        match self.0.reason.load(Ordering::SeqCst) {
            TIMED_OUT => Err(TingError::Timeout(
                "JS execution exceeded its deadline; instance is unavailable".into(),
            )),
            MEMORY_EXCEEDED => Err(TingError::ResourceLimitExceeded(
                "JS memory budget exceeded; instance is unavailable".into(),
            )),
            CANCELLED => Err(TingError::PluginExecutionError(
                "JS invocation was cancelled; instance is unavailable".into(),
            )),
            _ => Ok(()),
        }
    }

    pub fn unavailable(&self) -> bool {
        self.0.reason.load(Ordering::SeqCst) != 0
    }
}

impl BudgetState {
    fn reserve(&self, length: usize) -> bool {
        if self.reason.load(Ordering::SeqCst) != 0 {
            return false;
        }
        let mut current = self.external_bytes.load(Ordering::SeqCst);
        loop {
            let Some(total) = current
                .checked_add(length)
                .filter(|total| *total <= self.external_limit)
            else {
                break;
            };
            match self.external_bytes.compare_exchange_weak(
                current,
                total,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
        self.reason
            .compare_exchange(0, MEMORY_EXCEEDED, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        if let Some(handle) = self.isolate.lock().unwrap().as_ref() {
            handle.terminate_execution();
        }
        false
    }
}

// Allocator callbacks must never unwind across the C++ boundary.
unsafe extern "C" fn allocate(state: &BudgetState, length: usize) -> *mut c_void {
    if !state.reserve(length) {
        return std::ptr::null_mut();
    }
    let pointer = unsafe { libc::calloc(length.max(1), 1) };
    if pointer.is_null() {
        state.external_bytes.fetch_sub(length, Ordering::SeqCst);
    }
    pointer
}

unsafe extern "C" fn allocate_uninitialized(state: &BudgetState, length: usize) -> *mut c_void {
    if !state.reserve(length) {
        return std::ptr::null_mut();
    }
    let pointer = unsafe { libc::malloc(length.max(1)) };
    if pointer.is_null() {
        state.external_bytes.fetch_sub(length, Ordering::SeqCst);
    }
    pointer
}

unsafe extern "C" fn free(state: &BudgetState, pointer: *mut c_void, length: usize) {
    unsafe { libc::free(pointer) };
    state.external_bytes.fetch_sub(length, Ordering::SeqCst);
}

unsafe extern "C" fn reallocate(
    state: &BudgetState,
    pointer: *mut c_void,
    old_length: usize,
    new_length: usize,
) -> *mut c_void {
    let growth = new_length.saturating_sub(old_length);
    if !state.reserve(growth) {
        return std::ptr::null_mut();
    }
    let resized = unsafe { libc::realloc(pointer, new_length.max(1)) };
    if resized.is_null() {
        state.external_bytes.fetch_sub(growth, Ordering::SeqCst);
    } else if old_length > new_length {
        state
            .external_bytes
            .fetch_sub(old_length - new_length, Ordering::SeqCst);
    }
    resized
}

unsafe extern "C" fn allocator_drop(pointer: *const BudgetState) {
    // SAFETY: balances the Arc::into_raw in create_params.
    drop(unsafe { Arc::from_raw(pointer) });
}

extern "C" fn drop_chunk(pointer: *mut c_void, length: usize, owner: *mut c_void) {
    // SAFETY: copy_chunk transferred these two owned allocations to V8.
    let budget = unsafe { Arc::from_raw(owner.cast::<BudgetState>()) };
    unsafe { free(&budget, pointer, length) };
}

static ALLOCATOR: v8::RustAllocatorVtable<BudgetState> = v8::RustAllocatorVtable {
    allocate,
    allocate_uninitialized,
    free,
    reallocate,
    drop: allocator_drop,
};

struct ClockState {
    stopped: bool,
    generation: u64,
    deadline: Option<Instant>,
    cancellation: Option<CancellationToken>,
}

struct Clock {
    state: Mutex<ClockState>,
    changed: Condvar,
}

pub(super) struct ExecutionMonitor {
    clock: Arc<Clock>,
    budget: JsBudget,
    thread: Option<JoinHandle<()>>,
}

impl ExecutionMonitor {
    pub fn new(budget: JsBudget) -> Result<Self> {
        let clock = Arc::new(Clock {
            state: Mutex::new(ClockState {
                stopped: false,
                generation: 0,
                deadline: None,
                cancellation: None,
            }),
            changed: Condvar::new(),
        });
        let worker_clock = clock.clone();
        let worker_budget = budget.clone();
        let thread = std::thread::Builder::new()
            .name("js-budget".into())
            .spawn(move || {
                let mut state = worker_clock.state.lock().unwrap();
                while !state.stopped {
                    if let Some(deadline) = state.deadline {
                        if let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                            state = worker_clock
                                .changed
                                .wait_timeout(state, remaining)
                                .unwrap()
                                .0;
                        } else {
                            // Serialize expiry with disarming/rearming so a stale
                            // timer cannot terminate a later, healthy invocation.
                            worker_budget.terminate(TIMED_OUT);
                            if let Some(token) = state.cancellation.take() {
                                token.cancel();
                            }
                            state.deadline = None;
                        }
                    } else {
                        state = worker_clock.changed.wait(state).unwrap();
                    }
                }
            })?;
        Ok(Self {
            clock,
            budget,
            thread: Some(thread),
        })
    }

    pub fn begin(&self, timeout: Duration) -> Result<ExecutionLease> {
        self.budget.check()?;
        let mut state = self.clock.state.lock().unwrap();
        state.generation += 1;
        let generation = state.generation;
        let cancellation = CancellationToken::new();
        state.deadline = Some(Instant::now() + timeout);
        state.cancellation = Some(cancellation.clone());
        self.clock.changed.notify_one();
        Ok(ExecutionLease {
            clock: self.clock.clone(),
            budget: self.budget.clone(),
            generation,
            cancellation,
            completed: false,
        })
    }

    pub fn time_out(&self) {
        self.budget.terminate(TIMED_OUT);
    }
}

impl Drop for ExecutionMonitor {
    fn drop(&mut self) {
        {
            let mut state = self.clock.state.lock().unwrap();
            state.stopped = true;
            state.deadline = None;
            if let Some(token) = state.cancellation.take() {
                token.cancel();
            }
            self.clock.changed.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) struct ExecutionLease {
    clock: Arc<Clock>,
    budget: JsBudget,
    generation: u64,
    pub cancellation: CancellationToken,
    completed: bool,
}

impl ExecutionLease {
    pub fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for ExecutionLease {
    fn drop(&mut self) {
        if !self.completed {
            self.budget.terminate(CANCELLED);
        }
        self.cancellation.cancel();
        let mut state = self.clock.state.lock().unwrap();
        if state.generation == self.generation {
            state.deadline = None;
            state.cancellation = None;
            self.clock.changed.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/js/limits.rs"]
mod tests;
