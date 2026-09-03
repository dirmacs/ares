//! Fiber-scoped timer primitives for the Cordis kernel.
//!
//! Six upstream-style primitives, each registered as an *effect* on the
//! owning fiber so the kernel's LIFO disposal reclaims them:
//!
//! | Primitive | Shape |
//! |---|---|
//! | [`timeout`] | one-shot delay → callback → [`EffectHandle`] |
//! | [`sleep`] | one-shot delay → cancellable future |
//! | [`interval`] | repeating delay → callback → [`EffectHandle`] |
//! | [`interval_stream`] | repeating delay → [`Interval`] stream |
//! | [`debounce`] | trailing-edge collapse of bursts → [`Scheduled`]`<T>` |
//! | [`throttle`] | leading + optional trailing edge rate limit → [`Scheduled`]`<T>` |
//!
//! # Runtime expectations
//!
//! Timing runs on a **dedicated shared timer thread** (`cordis-timer`),
//! never on the owning fiber's task: the thread sleeps until the nearest
//! deadline in the shared wheel, drains every due entry under one short
//! critical section, then runs callbacks outside the wheel lock. Wheel
//! entries are one-shot; repeating registrations re-arm themselves from
//! inside their own callback. Callbacks must be cheap and non-blocking —
//! a stuck callback delays later firings but cannot kill the thread
//! (panics are caught and logged; the thread survives). The [`Interval`]
//! stream is polled by its owner: ticks queue in a channel while nobody
//! polls, and after disposal the stream yields exactly ONE final
//! `Err(InactiveEffect)` item before closing.
//!
//! # Fiber scoping
//!
//! Every registration made inside a fiber scope (see
//! [`with_current_fiber`]) pushes an undo onto that fiber via
//! [`Fiber::push_undo_labeled`]; when the fiber is disposed
//! ([`Fiber::dispose`]) or reactively passes through `Unloading` (effects
//! disposed LIFO), the undo cancels that registration. Dropping an
//! [`EffectHandle`] does NOT cancel anything — dispose it explicitly. A
//! registration made outside any fiber scope logs a warning and returns an
//! orphan handle whose explicit disposal still works but which no fiber
//! cancels automatically.
//!
//! ```no_run
//! use cordis::timer::{timeout, with_current_fiber};
//! use cordis::{Context, Fiber};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! let ctx = Context::new_root();
//! let fiber = Arc::new(Fiber::new());
//! let handle = with_current_fiber(&fiber, || {
//!     timeout(Duration::from_millis(10), || println!("fired once"))
//! });
//! // ...later, from async code: fiber.dispose().await cancels the timer.
//! ```

use std::cell::RefCell;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, LazyLock, Weak};
use std::task::{Context as TaskContext, Poll, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::effect::Disposable;
use crate::fiber::{Fiber, UndoMeta};

// ---------------------------------------------------------------------------
// Current-fiber scope
// ---------------------------------------------------------------------------

thread_local! {
    /// Weak reference to the fiber that timer registrations attach to on
    /// this thread. Weak is deliberate: registering a timer must not extend
    /// the fiber's lifetime, and a dead fiber has nothing left to cancel.
    static CURRENT_FIBER: RefCell<Option<Weak<Fiber>>> = const { RefCell::new(None) };
}

/// Run `f` with `fiber` installed as the current timer scope, restoring the
/// previous scope afterwards. Registrations inside `f` push their undo onto
/// `fiber`, so [`Fiber::dispose`] cancels them.
pub fn with_current_fiber<R>(fiber: &Arc<Fiber>, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT_FIBER.with(|slot| slot.borrow_mut().replace(Arc::downgrade(fiber)));
    let out = f();
    CURRENT_FIBER.with(|slot| *slot.borrow_mut() = prev);
    out
}

/// Read (and upgrade) the current fiber scope WITHOUT consuming it: several
/// primitives may register under one scope. A dead fiber upgrades to `None`,
/// which degrades that registration to the orphan path.
fn current_fiber_scope() -> Option<Arc<Fiber>> {
    CURRENT_FIBER
        .with(|slot| slot.borrow().clone())
        .and_then(|weak| weak.upgrade())
}

// ---------------------------------------------------------------------------
// Shared timer wheel
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// One scheduled one-shot entry. Ordered by deadline (min-heap via reversed
/// [`Ord`]); ties break on `seq` so ordering is total and deterministic.
struct Entry {
    deadline: Instant,
    job: Job,
    seq: u64,
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // BinaryHeap is a max-heap; reverse so earliest deadline pops first.
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Eq for Entry {}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.seq == other.seq
    }
}

#[derive(Default)]
struct Wheel {
    heap: BinaryHeap<Entry>,
}

static WHEEL: LazyLock<Arc<Mutex<Wheel>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Wheel::default())));

static TIMER_THREAD: LazyLock<JoinHandle<()>> = LazyLock::new(|| {
    std::thread::Builder::new()
        .name("cordis-timer".into())
        .spawn(run_timer_thread)
        .expect("spawn cordis-timer thread")
});

static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

/// Insert one entry into the shared wheel, spawning/unparking the shared
/// timer thread as needed.
fn schedule(deadline: Instant, job: Job) {
    let entry = Entry {
        deadline,
        job,
        seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
    };
    WHEEL.lock().heap.push(entry);
    timer_thread().thread().unpark();
}

fn timer_thread() -> &'static JoinHandle<()> {
    &TIMER_THREAD
}

fn run_timer_thread() {
    loop {
        // Phase 1 — read the nearest deadline WITHOUT holding the lock, then
        // sleep until it (or until an unpark announces an earlier insert).
        let sleep_for: Option<Duration> = {
            let w = WHEEL.lock();
            w.heap
                .peek()
                .map(|top| top.deadline.saturating_duration_since(Instant::now()))
        };
        if sleep_for != Some(Duration::ZERO) {
            match sleep_for {
                Some(d) => std::thread::park_timeout(d),
                None => std::thread::park(),
            }
        }
        // Phase 2 — drain every due entry under one short critical section;
        // callbacks run AFTER the lock is released so they can schedule new
        // entries (self-re-arming intervals, debounce/throttle emits)
        // without reentrant deadlocks.
        let due: Vec<Job> = {
            let mut w = WHEEL.lock();
            let now = Instant::now();
            let mut fired = Vec::new();
            while let Some(top) = w.heap.peek() {
                if top.deadline > now {
                    break;
                }
                fired.push(w.heap.pop().expect("peeked entry exists").job);
            }
            fired
        };
        for job in due {
            catch_panic(job);
        }
    }
}

fn catch_panic(job: Job) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    if let Err(err) = result {
        tracing::warn!(error = ?err, "cordis timer callback panicked; timer thread survives");
    }
}

// ---------------------------------------------------------------------------
// Handles and cancellation
// ---------------------------------------------------------------------------

struct HandleInner {
    disposed: AtomicBool,
    /// Optional extra teardown run exactly once at cancellation (waking a
    /// sleeping/stream waiter, invalidating armed jobs). Plain timers leave
    /// it empty.
    on_dispose: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Default for HandleInner {
    fn default() -> Self {
        Self {
            disposed: AtomicBool::new(false),
            on_dispose: Mutex::new(None),
        }
    }
}

impl HandleInner {
    fn set_on_dispose(&self, hook: Box<dyn Fn() + Send + Sync>) {
        *self.on_dispose.lock() = Some(hook);
    }

    /// Idempotent cancellation: flips the flag once, then runs the hook.
    fn trigger_dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(hook) = self.on_dispose.lock().take() {
            hook();
        }
    }
}

/// Effect handle for a registered timer primitive.
///
/// Clones share one cancellation flag; cancelling any clone cancels the
/// registration. Cancellation is ALSO pushed onto the owning fiber as a
/// labeled undo, so kernel-driven fiber teardown cancels timers without
/// caller action. Dropping the handle does NOT cancel.
pub struct EffectHandle {
    inner: Arc<HandleInner>,
}

impl Clone for EffectHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl EffectHandle {
    /// True once this registration was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.disposed.load(Ordering::Acquire)
    }

    fn cancelled(&self) -> bool {
        self.inner.disposed.load(Ordering::Acquire)
    }
}

impl Disposable for EffectHandle {
    fn dispose(self: Box<Self>) {
        self.inner.trigger_dispose();
    }
}

// ---------------------------------------------------------------------------
// Registration plumbing
// ---------------------------------------------------------------------------

/// Undo label prefix used for every timer effect pushed onto a fiber.
const UNDO_LABEL_PREFIX: &str = "timer:";

/// Cancel the handle produced by `register` through the current fiber scope
/// (if any). The undo closure owns a clone of the handle and disposes it
/// when the fiber's undo stack unwinds — through full [`Fiber::dispose`] or
/// a reactive pass through `Unloading`.
fn scoped_registration(label: &str, register: impl FnOnce() -> EffectHandle) -> EffectHandle {
    let handle = register();
    match current_fiber_scope() {
        Some(fiber) => {
            let meta = UndoMeta::new(format!("{UNDO_LABEL_PREFIX}{label}"));
            let dispose_handle = handle.clone();
            fiber.push_undo_labeled(
                meta,
                Box::new(move || Disposable::dispose(Box::new(dispose_handle))),
            );
        }
        None => tracing::warn!(
            label = %label,
            "cordis timer registered outside a fiber scope; nothing will auto-cancel it"
        ),
    }
    handle
}

// ---------------------------------------------------------------------------
// timeout / sleep / interval / interval_stream
// ---------------------------------------------------------------------------

/// One-shot: run `callback` after `delay` on the timer thread.
///
/// Returns an [`EffectHandle`] tied to the owning fiber; cancellation
/// (explicit or via fiber teardown) prevents the callback from ever running.
pub fn timeout<F>(delay: Duration, callback: F) -> EffectHandle
where
    F: FnOnce() + Send + 'static,
{
    scoped_registration("timeout", || {
        let flag = Arc::new(HandleInner::default());
        let fire_flag = flag.clone();
        schedule(
            Instant::now() + delay,
            Box::new(move || {
                // Flag check guards the race where cancellation lands after
                // the drain picked this entry up but before it ran.
                if !fire_flag.disposed.load(Ordering::Acquire) {
                    callback();
                }
            }),
        );
        EffectHandle { inner: flag }
    })
}

struct SleepState {
    done: AtomicBool,
    cancelled: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl SleepState {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn resolved(&self) -> bool {
        self.done.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire)
    }
}

/// One-shot wait without a callback: the future resolves after `delay`, or
/// early (silently) when the returned [`EffectHandle`] is disposed first.
///
/// The future is driven by its owner; the timer side only flips the
/// completion flag on the shared thread.
pub fn sleep(delay: Duration) -> (EffectHandle, impl Future<Output = ()> + Send) {
    let state = Arc::new(SleepState::new());
    let handle = scoped_registration("sleep", || {
        let job_state = state.clone();
        schedule(
            Instant::now() + delay,
            Box::new(move || {
                job_state.done.store(true, Ordering::Release);
                if let Some(wk) = job_state.waker.lock().take() {
                    wk.wake();
                }
            }),
        );
        let inner = Arc::new(HandleInner::default());
        let hook_state = state.clone();
        inner.set_on_dispose(Box::new(move || {
            hook_state.cancelled.store(true, Ordering::Release);
            if let Some(wk) = hook_state.waker.lock().take() {
                wk.wake();
            }
        }));
        EffectHandle { inner }
    });

    let fut_state = state;
    (handle, async move {
        core::future::poll_fn(move |cx| {
            if fut_state.resolved() {
                return Poll::Ready(());
            }
            *fut_state.waker.lock() = Some(cx.waker().clone());
            // Re-check after registering to close the lost-wakeup race.
            if fut_state.resolved() {
                return Poll::Ready(());
            }
            Poll::Pending
        })
        .await;
    })
}

/// Repeating: run `callback` every `delay`. Each tick re-arms the NEXT tick
/// from the moment it fires (cadence never runs ahead of the callback).
pub fn interval<F>(delay: Duration, callback: F) -> EffectHandle
where
    F: FnMut() + Send + 'static,
{
    scoped_registration("interval", || {
        let flag = Arc::new(HandleInner::default());
        // The callback sits in a shared cell so the self-re-arming job can
        // invoke it by mutable borrow each tick; a panicking callback leaves
        // the cell intact and simply stops further re-arming.
        type CallbackCell = Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>;
        let cell: CallbackCell = Arc::new(Mutex::new(Some(Box::new(callback))));
        fn rearm(flag: Arc<HandleInner>, cell: CallbackCell, delay: Duration) {
            schedule(
                Instant::now() + delay,
                Box::new(move || {
                    if flag.disposed.load(Ordering::Acquire) {
                        return; // chain ends; nothing re-armed
                    }
                    {
                        let mut guard = cell.lock();
                        if let Some(cb) = guard.as_mut() {
                            cb();
                        }
                    }
                    rearm(flag, cell, delay);
                }),
            );
        }
        rearm(flag.clone(), cell, delay);
        EffectHandle { inner: flag }
    })
}

/// Sentinel error yielded once by a disposed [`Interval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactiveEffect;

impl std::fmt::Display for InactiveEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timer effect went inactive (disposed)")
    }
}
impl std::error::Error for InactiveEffect {}

/// Item type of the [`Interval`] stream: `Ok(())` per tick, then exactly one
/// final `Err(InactiveEffect)` after disposal, then end-of-stream.
pub type TickResult = Result<(), InactiveEffect>;

/// Minimal single-item async stream trait mirroring
/// `futures_core::Stream::poll_next`; implemented by [`Interval`]. cordis
/// stays dependency-free; a futures-core adapter can wrap it externally.
pub trait Stream {
    /// Item type yielded by the stream.
    type Item;
    /// Yield the next item, or `None` once the stream has ended.
    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>>;
}

enum Tick {
    Fire,
    FinalErr,
}

/// Tick stream from [`interval_stream`]: `Ok(())` per elapsed tick, exactly
/// ONE `Err(InactiveEffect)` after disposal, then the stream stays closed.
/// Ticks queue in an unbounded channel while nobody polls, so slow
/// consumers observe every live tick (no coalescing); live ticks queued
/// before a disposal are discarded so teardown is always the final
/// observation.
pub struct Interval {
    rx: Receiver<Tick>,
    state: Arc<HandleInner>,
    waker: Arc<Mutex<Option<Waker>>>,
    final_emitted: AtomicBool,
}

impl Interval {
    /// True once this stream's registration was disposed.
    pub fn is_cancelled(&self) -> bool {
        self.state.disposed.load(Ordering::Acquire)
    }
}

impl Drop for Interval {
    fn drop(&mut self) {
        // Dropping the stream stops scheduling; idempotent if already torn
        // down through the fiber.
        self.state.trigger_dispose();
    }
}

impl Stream for Interval {
    type Item = TickResult;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        // SAFETY: structural pin projection over owned fields; nothing is
        // moved out of `self`, so `get_unchecked_mut` cannot violate the
        // pinning guarantee here.
        let this = unsafe { self.get_unchecked_mut() };
        loop {
            match this.rx.try_recv() {
                Ok(Tick::Fire) => {
                    if this.state.disposed.load(Ordering::Acquire) {
                        continue; // discard ticks queued before disposal
                    }
                    return Poll::Ready(Some(Ok(())));
                }
                Ok(Tick::FinalErr) => {
                    if this.final_emitted.swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    return Poll::Ready(Some(Err(InactiveEffect)));
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {
                    if this.final_emitted.load(Ordering::Acquire) {
                        return Poll::Ready(None);
                    }
                    if this.state.disposed.load(Ordering::Acquire) {
                        // Drain any residual items so the final error is the
                        // last observation.
                        while matches!(this.rx.try_recv(), Ok(Tick::Fire)) {}
                        if !this.final_emitted.swap(true, Ordering::AcqRel) {
                            return Poll::Ready(Some(Err(InactiveEffect)));
                        }
                        return Poll::Ready(None);
                    }
                    // Live wait: register, then double-check for the race
                    // where a tick/disposal landed between try_recv and now.
                    *this.waker.lock() = Some(cx.waker().clone());
                    if this.state.disposed.load(Ordering::Acquire) {
                        continue;
                    }
                    match this.rx.try_recv() {
                        Ok(Tick::FinalErr) => {
                            if !this.final_emitted.swap(true, Ordering::AcqRel) {
                                return Poll::Ready(Some(Err(InactiveEffect)));
                            }
                        }
                        Ok(Tick::Fire) => return Poll::Ready(Some(Ok(()))),
                        Err(_) => return Poll::Pending,
                    }
                }
            }
        }
    }
}

/// Repeating registration returning a stream instead of running a callback.
///
/// The returned [`Interval`] is polled by its owner; disposal via the
/// owning fiber's undo stack produces the single final
/// `Err(InactiveEffect)` and closes the stream.
pub fn interval_stream(delay: Duration) -> Interval {
    let (tx, rx) = std::sync::mpsc::channel::<Tick>();
    let flag = Arc::new(HandleInner::default());
    let waker_slot: Arc<Mutex<Option<Waker>>> = Arc::new(Mutex::new(None));

    scoped_registration("interval_stream", || {
        fn rearm(
            flag: Arc<HandleInner>,
            tx: Sender<Tick>,
            waker_slot: Arc<Mutex<Option<Waker>>>,
            delay: Duration,
        ) {
            schedule(
                Instant::now() + delay,
                Box::new(move || {
                    if flag.disposed.load(Ordering::Acquire) {
                        // Chain ends: announce teardown exactly once so a
                        // parked poller observes the final error promptly.
                        let _ = tx.send(Tick::FinalErr);
                        if let Some(wk) = waker_slot.lock().take() {
                            wk.wake();
                        }
                        return;
                    }
                    let _ = tx.send(Tick::Fire);
                    if let Some(wk) = waker_slot.lock().take() {
                        wk.wake();
                    }
                    rearm(flag, tx, waker_slot, delay);
                }),
            );
        }
        // Dispose hook: same prompt-teardown announcement for explicit
        // cancellation between ticks.
        let hook_flag = flag.clone();
        let hook_tx = tx.clone();
        let hook_waker = waker_slot.clone();
        flag.set_on_dispose(Box::new(move || {
            hook_flag.disposed.store(true, Ordering::Release);
            let _ = hook_tx.send(Tick::FinalErr);
            if let Some(wk) = hook_waker.lock().take() {
                wk.wake();
            }
        }));
        rearm(flag.clone(), tx, waker_slot.clone(), delay);
        EffectHandle {
            inner: flag.clone(),
        }
    });

    Interval {
        rx,
        state: flag,
        waker: waker_slot,
        final_emitted: AtomicBool::new(false),
    }
}

// ---------------------------------------------------------------------------
// Debounce / throttle
// ---------------------------------------------------------------------------

/// Emitter/consumer pair for [`debounce`] and [`throttle`]: call
/// [`Scheduled::call`] with each burst value; collapsed/rate-limited
/// deliveries reach the consumer through [`Scheduled::receive`] /
/// [`Scheduled::receive_timeout`]. Cancelling the paired handle (explicitly
/// or through fiber teardown) drops pending values; receives then return
/// `None`.
///
/// The consumer side is synchronous by design: `receive_timeout` parks the
/// calling thread, which keeps the primitives usable from plain threads and
/// from `block_on` style glue alike.
pub struct Scheduled<T> {
    tx: Sender<T>,
    rx: Receiver<T>,
    handle: EffectHandle,
    submit: Box<dyn Fn(T) + Send + Sync>,
}

impl<T> Scheduled<T> {
    /// Submit one value into the burst window. No-op once cancelled.
    pub fn call(&self, value: T) {
        if self.handle.cancelled() {
            return;
        }
        (self.submit)(value);
    }

    /// Non-blocking receive of the next delivered value; `None` when nothing
    /// is pending or the emitter was cancelled/drained.
    pub fn receive(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }

    /// Blocking receive bounded by `timeout`; `None` on timeout or once the
    /// emitter is cancelled and drained.
    pub fn receive_timeout(&mut self, timeout: Duration) -> Option<T> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// True once this emitter was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.handle.is_cancelled()
    }

    /// Explicitly cancel this emitter (same effect as fiber disposal).
    pub fn cancel(&self) {
        self.handle.inner.trigger_dispose();
    }
}

impl<T: Send + 'static> Disposable for Scheduled<T> {
    fn dispose(self: Box<Self>) {
        self.handle.inner.trigger_dispose();
    }
}

/// Collapse a burst of [`Scheduled::call`]s into ONE trailing delivery,
/// emitted `delay` after the LAST call of the burst (sliding quiet window).
/// Every call replaces the pending value and re-arms the emit deadline;
/// superseded emit jobs observe a stale generation and no-op.
pub fn debounce<T: Send + 'static>(delay: Duration) -> Scheduled<T> {
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    let flag = Arc::new(HandleInner::default());
    let latest: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let generation = Arc::new(AtomicU64::new(0));

    let handle = scoped_registration("debounce", || {
        // Disposal invalidates armed jobs and drops any pending value so a
        // late emit delivers nothing.
        let hook_latest = latest.clone();
        let hook_gen = generation.clone();
        let hook_flag = flag.clone();
        flag.set_on_dispose(Box::new(move || {
            hook_flag.disposed.store(true, Ordering::Release);
            *hook_latest.lock() = None;
            hook_gen.fetch_add(1, Ordering::SeqCst);
        }));
        EffectHandle {
            inner: flag.clone(),
        }
    });

    let submit = {
        let latest = latest.clone();
        let generation = generation.clone();
        let flag = flag.clone();
        let tx = tx.clone();
        Box::new(move |value: T| {
            if flag.disposed.load(Ordering::Acquire) {
                return;
            }
            *latest.lock() = Some(value);
            let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
            let emit_latest = latest.clone();
            let emit_gen = generation.clone();
            let emit_tx = tx.clone();
            let emit_flag = flag.clone();
            schedule(
                Instant::now() + delay,
                Box::new(move || {
                    // Stale job: a newer call re-armed the window.
                    if emit_gen.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    if emit_flag.disposed.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(v) = emit_latest.lock().take() {
                        let _ = emit_tx.send(v);
                    }
                }),
            );
        }) as Box<dyn Fn(T) + Send + Sync>
    };

    Scheduled {
        tx,
        rx,
        handle,
        submit,
    }
}

/// Rate limiter with leading edge plus optional trailing edge.
///
/// With `no_trailing = false`: the FIRST value of a quiet-period burst
/// delivers immediately (leading edge) and the LAST value received during
/// the window delivers at window end (trailing edge, `delay` after the
/// leading delivery). With `no_trailing = true`: leading deliveries only;
/// values arriving during the window are dropped. The window itself is
/// fixed-length: it always closes `delay` after the leading delivery so a
/// fresh burst can lead again.
pub fn throttle<T: Send + 'static>(delay: Duration, no_trailing: bool) -> Scheduled<T> {
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    let flag = Arc::new(HandleInner::default());
    let pending: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
    let window_open = Arc::new(AtomicBool::new(false));

    let handle = scoped_registration("throttle", || {
        // Disposal drops any pending trailing value; the window flag resets
        // so a cancelled emitter leaves nothing behind.
        let hook_pending = pending.clone();
        let hook_window = window_open.clone();
        let hook_flag = flag.clone();
        flag.set_on_dispose(Box::new(move || {
            hook_flag.disposed.store(true, Ordering::Release);
            *hook_pending.lock() = None;
            hook_window.store(false, Ordering::Release);
        }));
        EffectHandle {
            inner: flag.clone(),
        }
    });

    let submit = {
        let pending = pending.clone();
        let window_open = window_open.clone();
        let flag = flag.clone();
        let tx = tx.clone();
        Box::new(move |value: T| {
            if flag.disposed.load(Ordering::Acquire) {
                return;
            }
            if window_open.swap(true, Ordering::SeqCst) {
                // Window active: keep only the newest value for the trailing
                // edge (which no_trailing discards at window close anyway).
                *pending.lock() = Some(value);
                return;
            }
            // Leading edge passes immediately; the close job always runs to
            // reopen the window, delivering the trailing value only when
            // requested.
            let _ = tx.send(value);
            let close_pending = pending.clone();
            let close_window = window_open.clone();
            let close_flag = flag.clone();
            let close_tx = tx.clone();
            schedule(
                Instant::now() + delay,
                Box::new(move || {
                    close_window.store(false, Ordering::Release);
                    if close_flag.disposed.load(Ordering::Acquire) || no_trailing {
                        return;
                    }
                    if let Some(v) = close_pending.lock().take() {
                        let _ = close_tx.send_for_throttle_trailing(v);
                    }
                }),
            );
        }) as Box<dyn Fn(T) + Send + Sync>
    };

    Scheduled {
        tx,
        rx,
        handle,
        submit,
    }
}

/// Tiny helper keeping the trailing-edge send site readable; identical to
/// `Sender::send` apart from naming the intent.
trait SendForThrottleTrailing<T> {
    fn send_for_throttle_trailing(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>>;
}

impl<T> SendForThrottleTrailing<T> for Sender<T> {
    fn send_for_throttle_trailing(&self, value: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        self.send(value)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `fut` to completion on the current thread with a bounded
    /// overall budget so a regression fails fast instead of hanging CI.
    fn block_on_bounded<F: Future>(fut: F, budget: Duration) -> F::Output {
        let started = Instant::now();
        let waker = Waker::noop();
        let mut cx = TaskContext::from_waker(waker);
        let mut fut = Box::pin(fut);
        loop {
            match fut.as_mut().poll(&mut cx) {
                Poll::Ready(v) => return v,
                Poll::Pending => {
                    assert!(
                        started.elapsed() <= budget,
                        "future did not resolve within {budget:?}"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }

    /// Single-threaded stream poll; `None` means "not ready yet".
    fn poll_stream_once(stream: &mut Interval) -> Option<TickResult> {
        let waker = Waker::noop();
        let mut cx = TaskContext::from_waker(waker);
        match Stream::poll_next(Pin::new(stream), &mut cx) {
            Poll::Ready(item) => item,
            Poll::Pending => None,
        }
    }

    #[test]
    fn timeout_fires_once_and_disposes_with_fiber() {
        let fiber = Arc::new(Fiber::new());
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        let handle = with_current_fiber(&fiber, || {
            timeout(Duration::from_millis(20), move || {
                h.fetch_add(1, Ordering::SeqCst);
            })
        });
        assert!(!handle.is_cancelled());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "nothing fired before the deadline"
        );

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "callback must run exactly once"
        );

        // Disposal via the owning fiber cancels the effect.
        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();
        assert!(handle.is_cancelled(), "fiber disposal must cancel timers");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "no extra fire after disposal"
        );
    }

    #[test]
    fn timeout_dispose_before_deadline_prevents_fire() {
        let fiber = Arc::new(Fiber::new());
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        let handle = with_current_fiber(&fiber, || {
            timeout(Duration::from_millis(60), move || {
                h.fetch_add(1, Ordering::SeqCst);
            })
        });
        Disposable::dispose(Box::new(handle));
        std::thread::sleep(Duration::from_millis(90));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "disposed timeout must never fire"
        );
    }

    #[test]
    fn sleep_resolves_and_dispose_resolves_early() {
        let fiber = Arc::new(Fiber::new());
        let (handle, fut) = with_current_fiber(&fiber, || sleep(Duration::from_millis(30)));
        block_on_bounded(fut, Duration::from_secs(2));
        assert!(!handle.is_cancelled());

        // Early-exit path: disposal resolves a pending sleep immediately.
        let fiber2 = Arc::new(Fiber::new());
        let (handle2, fut2) = with_current_fiber(&fiber2, || sleep(Duration::from_millis(500)));
        let started = Instant::now();
        let poller = std::thread::spawn(move || {
            block_on_bounded(fut2, Duration::from_secs(2));
            started.elapsed()
        });
        std::thread::sleep(Duration::from_millis(30));
        Disposable::dispose(Box::new(handle2));
        let elapsed = poller.join().expect("poller thread");
        assert!(
            elapsed < Duration::from_millis(400),
            "disposal must resolve the pending sleep early (took {elapsed:?})"
        );
    }

    #[test]
    fn interval_ticks_repeatedly_and_stops_on_dispose() {
        let fiber = Arc::new(Fiber::new());
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        let handle = with_current_fiber(&fiber, || {
            interval(Duration::from_millis(10), move || {
                h.fetch_add(1, Ordering::SeqCst);
            })
        });
        std::thread::sleep(Duration::from_millis(55));
        let count = hits.load(Ordering::SeqCst);
        assert!(count >= 2, "interval must tick repeatedly (got {count})");

        Disposable::dispose(Box::new(handle));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            count,
            "ticks must stop after disposal"
        );
    }

    #[test]
    fn interval_stream_final_err_on_dispose() {
        let fiber = Arc::new(Fiber::new());
        let mut stream = with_current_fiber(&fiber, || interval_stream(Duration::from_millis(10)));

        // Collect two live ticks.
        let mut live_ticks = 0u32;
        let deadline = Instant::now() + Duration::from_secs(2);
        while live_ticks < 2 {
            assert!(Instant::now() < deadline, "timed out collecting live ticks");
            if let Some(item) = poll_stream_once(&mut stream) {
                assert_eq!(item, Ok(()), "live ticks must be Ok");
                live_ticks += 1;
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        // Dispose through the owning fiber.
        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();

        // Exactly ONE final Err(InactiveEffect), then end-of-stream.
        let final_item =
            poll_stream_once(&mut stream).expect("final err item must arrive after disposal");
        assert_eq!(final_item, Err(InactiveEffect));
        assert!(
            poll_stream_once(&mut stream).is_none(),
            "stream must terminate after the final error"
        );
        assert!(stream.is_cancelled());
    }

    #[test]
    fn interval_stream_discards_stale_live_ticks_before_final_err() {
        let fiber = Arc::new(Fiber::new());
        let mut stream = with_current_fiber(&fiber, || interval_stream(Duration::from_millis(5)));

        // Accumulate several live ticks WITHOUT polling, then dispose; the
        // final observation must be the error, not a stale Ok.
        std::thread::sleep(Duration::from_millis(18));
        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();

        let mut saw_err = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match poll_stream_once(&mut stream) {
                Some(Err(InactiveEffect)) => {
                    saw_err = true;
                    break;
                }
                Some(Ok(())) => {} // stale live tick: keep draining
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        assert!(saw_err, "teardown must be observable as the final error");
        assert!(poll_stream_once(&mut stream).is_none());
    }

    #[test]
    fn debounce_collapses_bursts() {
        let fiber = Arc::new(Fiber::new());
        let mut sched = with_current_fiber(&fiber, || debounce::<u32>(Duration::from_millis(40)));

        // Burst: five calls inside the sliding quiet window (~16ms total).
        for i in 0..5 {
            sched.call(i);
            std::thread::sleep(Duration::from_millis(4));
        }

        // Only the LAST value survives the window.
        let delivered = sched.receive_timeout(Duration::from_secs(2));
        assert_eq!(
            delivered,
            Some(4),
            "debounce must deliver only the trailing value"
        );

        // Quiet period: no further deliveries.
        let extra = sched.receive_timeout(Duration::from_millis(120));
        assert_eq!(extra, None, "one burst collapses into exactly one delivery");

        // Fiber disposal cancels the emitter; later calls are no-ops.
        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();
        assert!(sched.is_cancelled());
        sched.call(9);
        assert_eq!(sched.receive_timeout(Duration::from_millis(50)), None);
    }

    #[test]
    fn throttle_trailing_edge_respected() {
        let fiber = Arc::new(Fiber::new());
        let mut sched =
            with_current_fiber(&fiber, || throttle::<u32>(Duration::from_millis(50), false));

        // Burst of five rapid calls (~16ms, inside one 50ms window).
        for i in 0..5 {
            sched.call(i);
            std::thread::sleep(Duration::from_millis(4));
        }

        let leading = sched.receive_timeout(Duration::from_secs(1));
        assert_eq!(leading, Some(0), "leading edge passes immediately");

        let trailing = sched.receive_timeout(Duration::from_secs(2));
        assert_eq!(
            trailing,
            Some(4),
            "trailing edge must respect the last value"
        );

        let extra = sched.receive_timeout(Duration::from_millis(120));
        assert_eq!(extra, None, "exactly leading + trailing per burst");

        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();
        assert!(sched.is_cancelled());
    }

    #[test]
    fn throttle_no_trailing_drops_rest_of_burst() {
        let fiber = Arc::new(Fiber::new());
        let mut sched =
            with_current_fiber(&fiber, || throttle::<u32>(Duration::from_millis(50), true));

        for i in 0..4 {
            sched.call(i);
            std::thread::sleep(Duration::from_millis(4));
        }

        assert_eq!(sched.receive_timeout(Duration::from_secs(1)), Some(0));
        assert_eq!(
            sched.receive_timeout(Duration::from_millis(150)),
            None,
            "no_trailing must drop every value after the leading one"
        );
    }

    #[test]
    fn fiber_death_cancels_all_timers() {
        let fiber = Arc::new(Fiber::new());

        let (t_handle, timeout_hits, interval_hits, i_handle, mut stream) =
            with_current_fiber(&fiber, || {
                let th = Arc::new(AtomicU64::new(0));
                let th2 = th.clone();
                let t = timeout(Duration::from_millis(70), move || {
                    th2.fetch_add(1, Ordering::SeqCst);
                });
                let ih = Arc::new(AtomicU64::new(0));
                let ih2 = ih.clone();
                let i = interval(Duration::from_millis(15), move || {
                    ih2.fetch_add(1, Ordering::SeqCst);
                });
                let s = interval_stream(Duration::from_millis(12));
                (t, th, ih, i, s)
            });
        let _ = (&t_handle, &i_handle);

        // Let the interval tick at least once BEFORE death; the 70ms timeout
        // must still be pending.
        std::thread::sleep(Duration::from_millis(50));
        let pre_interval_hits = interval_hits.load(Ordering::SeqCst);
        assert!(
            pre_interval_hits >= 1,
            "interval should tick before fiber death"
        );
        assert_eq!(
            timeout_hits.load(Ordering::SeqCst),
            0,
            "timeout still pending"
        );

        // Fiber death: every timer effect must be cancelled.
        block_on_bounded(fiber.dispose(), Duration::from_secs(2)).unwrap();

        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            timeout_hits.load(Ordering::SeqCst),
            0,
            "pending timeout must never fire after fiber death"
        );
        assert_eq!(
            interval_hits.load(Ordering::SeqCst),
            pre_interval_hits,
            "interval must stop ticking after fiber death"
        );

        // The surviving stream observes the teardown as the final error.
        let mut saw_final_err = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match poll_stream_once(&mut stream) {
                Some(Err(InactiveEffect)) => {
                    saw_final_err = true;
                    break;
                }
                Some(Ok(())) => {} // stale tick, keep polling
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        assert!(
            saw_final_err,
            "disposed interval_stream must yield Err(InactiveEffect)"
        );
        assert!(poll_stream_once(&mut stream).is_none(), "then terminate");
    }

    #[test]
    fn orphan_registration_warns_but_disposable() {
        // No fiber scope: registration warns and returns an orphan handle
        // whose explicit disposal still prevents firing.
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        let handle = timeout(Duration::from_millis(30), move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        Disposable::dispose(Box::new(handle));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn undo_labels_are_recorded_on_the_fiber() {
        let fiber = Arc::new(Fiber::new());
        with_current_fiber(&fiber, || {
            timeout(Duration::from_millis(500), || {});
            interval(Duration::from_millis(500), || {});
        });
        let labels = fiber.pending_undo_labels();
        assert!(
            labels.iter().any(|l| l.contains("timer:timeout")),
            "labels: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("timer:interval")),
            "labels: {labels:?}"
        );
    }
}
