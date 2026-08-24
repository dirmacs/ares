//! Metatheory property smoke tests for the Cordis kernel.
//!
//! Each public function encodes one guarantee from the Cordis conceptual
//! paper as an executable check over the public API surface
//! ([`Context::provide`](crate::Context::provide),
//! [`Context::remove`](crate::Context::remove),
//! [`Context::get`](crate::Context::get),
//! [`Context::plugin`](crate::Context::plugin),
//! [`RegistryService::register`](crate::RegistryService::register),
//! [`Fiber::refresh`](crate::Fiber::refresh),
//! [`Fiber::dispose`](crate::Fiber::dispose),
//! [`ReflectService::notify_with_ctx`](crate::ReflectService::notify_with_ctx)).
//! The functions return `Result<(), String>` so future harness binaries can
//! reuse them; the `#[tokio::test]` wrappers at the bottom are the CI entry
//! points.
//!
//! # Guarantees smoked
//!
//! * [`quiescence_after_every_op`] — Thm 66 (progress): outside an await
//!   point no registered fiber rests in a transitional state (`Loading`,
//!   `Reloading`, `Unloading`), and every `Active` fiber has all of its
//!   declared injects available.
//! * [`order_confluence_of_registrations`] — Cor 21 / Thm 73: registration
//!   order of independent plugins does not change the converged observable
//!   state (same epoch string, same projected values).
//! * [`dependent_never_active_without_provider`] — spatial reactive
//!   invariant: a dependent fiber is never `Active` while its provider is
//!   absent, activates reactively when the provider appears, tracks provider
//!   swaps through its epoch, and the guarded-withdrawal rule protects live
//!   consumers.
//! * [`lifo_dispose_restores_store`] — Thm 16: disposal unwinds registration
//!   effects in strict LIFO order and restores the store to its pre-
//!   registration contents.
//!
//! # Known deltas between paper and implementation (deliberate, verified)
//!
//! 1. `Fiber::declare_inject` mutates the inject set *outside* the fiber
//!    state machine. Between a declaration and the next refresh, an `Active`
//!    fiber can carry a freshly declared (unsatisfied) inject. The progress
//!    invariant therefore only holds if callers reconcile eagerly after each
//!    declaration — which every reactive driver (watcher, loader) does. These
//!    tests refresh synchronously right after declaring and document the
//!    discipline rather than weakening the check.
//! 2. A registration whose factory **errors** surfaces `Failed` *and* skips
//!    the reflective wiring (`RegistryService::register` returns `Err` before
//!    `provided.insert` and before `ReflectService::register_fiber`), so the
//!    fiber is not driven by later notifications and cannot be recovered
//!    through the public API (the id is never returned). The paper expects a
//!    permanently `Inactive` dependent instead. These tests use total
//!    factories gated by `Service::check`, which yields the paper-conform
//!    `Inactive` state; the fallible-factory behavior is reported as a delta,
//!    not silently weakened away.
//! 3. Transitional states are observable *mid-await* (see the `Reloading`
//!    poller test in `fiber.rs`), but never at rest: the fiber inertia mutex
//!    serializes transitions, so quiescence sampled between operations is
//!    well-defined.

use std::any::TypeId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::context::Context;
use crate::effect::Disposable;
use crate::fiber::{Fiber, FiberState};
use crate::registry::{Plugin, RegistryService};
use crate::service::{CordisError, Service, ServiceInitFuture};
use crate::ReflectService;

// ---------------------------------------------------------------------------
// Shared fixtures — plain services/plugins with disjoint TypeIds per scenario
// ---------------------------------------------------------------------------

/// Unrelated provider bumped/removed by the randomized schedule (scenario 1).
#[derive(Debug)]
struct MtU1(pub u64);
impl Service for MtU1 {}

/// Second unrelated provider, pure noise for the schedule (scenario 1).
#[derive(Debug)]
struct MtU2(pub u64);
impl Service for MtU2 {}

/// Independent service supplied by a plugin (scenario 1).
#[derive(Debug)]
struct MtI1(pub u64);
impl Service for MtI1 {}

/// Projection of `MtI1` + `MtU1`; `check()` reports dependency satisfaction.
#[derive(Debug)]
struct MtCVal {
    i1: u64,
    u1: u64,
    ready: bool,
}
impl Service for MtCVal {
    fn check(&self) -> bool {
        self.ready
    }
}

/// Total factory providing `MtI1`.
struct MtIndepPlugin;
impl Plugin for MtIndepPlugin {
    type Config = ();
    type Provides = MtI1;
    fn apply(&self, _ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtI1>, CordisError> {
        Ok(Arc::new(MtI1(7)))
    }
}

/// Total, `check`-gated factory consuming `MtI1` and `MtU1`.
struct MtConsumerPlugin;
impl Plugin for MtConsumerPlugin {
    type Config = ();
    type Provides = MtCVal;
    fn apply(&self, ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtCVal>, CordisError> {
        let i1 = ctx.get::<MtI1>().map(|v| v.0);
        let u1 = ctx.get::<MtU1>().map(|v| v.0);
        Ok(Arc::new(MtCVal {
            i1: i1.unwrap_or(0),
            u1: u1.unwrap_or(0),
            ready: i1.is_some() && u1.is_some(),
        }))
    }
}

/// Provider one for the confluence scenario (scenario 2).
#[derive(Debug)]
struct MtP1(pub u32);
impl Service for MtP1 {}

/// Provider two for the confluence scenario (scenario 2).
#[derive(Debug)]
struct MtP2(pub u32);
impl Service for MtP2 {}

/// Projection of both providers; `check()` reports satisfaction.
#[derive(Debug)]
struct MtPair {
    a: u32,
    b: u32,
    ready: bool,
}
impl Service for MtPair {
    fn check(&self) -> bool {
        self.ready
    }
}

struct MtP1Plugin;
impl Plugin for MtP1Plugin {
    type Config = ();
    type Provides = MtP1;
    fn apply(&self, _ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtP1>, CordisError> {
        Ok(Arc::new(MtP1(7)))
    }
}

struct MtP2Plugin;
impl Plugin for MtP2Plugin {
    type Config = ();
    type Provides = MtP2;
    fn apply(&self, _ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtP2>, CordisError> {
        Ok(Arc::new(MtP2(11)))
    }
}

struct MtPairPlugin;
impl Plugin for MtPairPlugin {
    type Config = ();
    type Provides = MtPair;
    fn apply(&self, ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtPair>, CordisError> {
        let a = ctx.get::<MtP1>().map(|v| v.0);
        let b = ctx.get::<MtP2>().map(|v| v.0);
        Ok(Arc::new(MtPair {
            a: a.unwrap_or(0),
            b: b.unwrap_or(0),
            ready: a.is_some() && b.is_some(),
        }))
    }
}

/// Provider whose value the dependent projects (scenario 3).
#[derive(Debug)]
struct MtProv(pub u32);
impl Service for MtProv {}

/// Dependent projection; `check()` reports satisfaction.
#[derive(Debug)]
struct MtDerived {
    src: u32,
    ready: bool,
}
impl Service for MtDerived {
    fn check(&self) -> bool {
        self.ready
    }
}

struct MtDependentPlugin;
impl Plugin for MtDependentPlugin {
    type Config = ();
    type Provides = MtDerived;
    fn apply(&self, ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtDerived>, CordisError> {
        match ctx.get::<MtProv>() {
            Some(v) => Ok(Arc::new(MtDerived {
                src: v.0,
                ready: true,
            })),
            None => Ok(Arc::new(MtDerived {
                src: 0,
                ready: false,
            })),
        }
    }
}

/// Directly provided marker whose removal must be undone on disposal.
#[derive(Debug)]
struct MtMark1(pub u64);
impl Service for MtMark1 {}

/// Second directly provided marker (scenario 4).
#[derive(Debug)]
struct MtMark2(pub u64);
impl Service for MtMark2 {}

/// Service whose `init` installs one disposable recording `"A"` (scenario 4).
struct MtEffA {
    log: Arc<Mutex<Vec<&'static str>>>,
    count: Arc<AtomicUsize>,
}
impl Service for MtEffA {
    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        let log = self.log.clone();
        let count = self.count.clone();
        Box::pin(async move {
            Ok(Some(Box::new(move || {
                log.lock().push("A");
                count.fetch_add(1, Ordering::SeqCst);
            }) as Box<dyn Disposable>))
        })
    }
}

/// Service whose `init` installs one disposable recording `"B"` (scenario 4).
struct MtEffB {
    log: Arc<Mutex<Vec<&'static str>>>,
    count: Arc<AtomicUsize>,
}
impl Service for MtEffB {
    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        let log = self.log.clone();
        let count = self.count.clone();
        Box::pin(async move {
            Ok(Some(Box::new(move || {
                log.lock().push("B");
                count.fetch_add(1, Ordering::SeqCst);
            }) as Box<dyn Disposable>))
        })
    }
}

// ---------------------------------------------------------------------------
// Harness helpers
// ---------------------------------------------------------------------------

/// Fresh root with `RegistryService` + `ReflectService` installed.
fn base_root() -> (Arc<Context>, Arc<RegistryService>, Arc<ReflectService>) {
    let ctx = Context::new_root();
    let reg = ctx.provide(RegistryService::new());
    let reflect = ctx.provide(ReflectService::new());
    (ctx, reg, reflect)
}

/// Yield enough times for any `ReflectService::notify` spawn (finite: one per
/// provide/remove) to finish, keeping observations race-free without sleeps.
async fn drain_spawned() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Thm 66 sample-at-rest: no fiber may sit in `Loading`/`Reloading`/
/// `Unloading` between operations, and every `Active` fiber must have all
/// declared injects available. Availability is probed with the crate-internal
/// `Context::is_available`, the exact predicate `Context::get` applies before
/// handing out a value.
fn assert_quiescent(fibers: &[(String, Arc<Fiber>)], ctx: &Arc<Context>) -> Result<(), String> {
    for (name, fiber) in fibers {
        match fiber.state() {
            FiberState::Loading | FiberState::Reloading | FiberState::Unloading { .. } => {
                return Err(format!(
                    "fiber '{name}' rests in transitional state {:?}",
                    fiber.state()
                ));
            }
            FiberState::Active { .. } => {
                for tid in fiber.injected_type_ids() {
                    if !ctx.is_available(tid) {
                        return Err(format!(
                            "fiber '{name}' is Active but injected dependency {tid:?} is unavailable"
                        ));
                    }
                }
            }
            FiberState::Inactive { .. } | FiberState::Failed { .. } => {}
        }
    }
    Ok(())
}

/// Deterministic xorshift64* — "random-ish" schedule, fixed seed, replayable.
struct MtRng(u64);

impl MtRng {
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

    fn new() -> Self {
        MtRng(Self::SEED)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x0254_5F49_14BF_49C9)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

// ---------------------------------------------------------------------------
// Property 1 — Thm 66 (progress / quiescence at rest)
// ---------------------------------------------------------------------------

/// Run a deterministic pseudo-random operation schedule over a fresh root and
/// assert the quiescence invariant after every single operation.
///
/// Operations drawn from a fixed table via xorshift64* (seeded, replayable):
/// provide/bump `MtU1`, provide `MtU2`, reactive notify on `MtU1`/`MtI1`,
/// register the independent plugin, register the consumer plugin (which
/// declares injects on `MtI1`+`MtU1` and reconciles synchronously), guarded
/// removal of `MtU1`, and disposal of the most recent registration. Duplicate
/// provider refusals and guarded-withdrawal refusals are legal outcomes and
/// their error text is validated; any other error fails the property.
///
/// After the schedule the harness forces convergence (all providers present,
/// reactive notify both directions) and asserts the consumer is `Active` with
/// an epoch equal to a fresh `compute_epoch` and a projection matching the
/// live store.
pub async fn quiescence_after_every_op() -> Result<(), String> {
    let (ctx, reg, reflect) = base_root();
    let mut rng = MtRng::new();
    let mut fibers: Vec<(String, Arc<Fiber>)> = vec![("root".to_string(), ctx.fiber())];
    let mut newest: Option<usize> = None;

    for step in 0..24u32 {
        match rng.below(8) {
            0 => {
                let v = rng.next() % 1000;
                ctx.provide(MtU1(v));
            }
            1 => {
                let v = rng.next() % 1000;
                ctx.provide(MtU2(v));
            }
            2 => {
                reflect.notify_with_ctx(TypeId::of::<MtU1>(), &ctx).await;
            }
            3 => {
                reflect.notify_with_ctx(TypeId::of::<MtI1>(), &ctx).await;
            }
            4 => match reg.register(&ctx, MtIndepPlugin, ()) {
                Ok(fid) => {
                    let fiber = reg.get_fiber(fid).expect("registered fiber tracked");
                    fibers.push((format!("indep-{fid}"), fiber));
                    newest = Some(fibers.len() - 1);
                }
                Err(e) => {
                    // Duplicate-provider refusals are the single-source
                    // discipline working; anything else is a violation.
                    if !e.to_string().contains("duplicate provider") {
                        return Err(format!("step {step}: unexpected register error: {e}"));
                    }
                }
            },
            5 => match reg.register(&ctx, MtConsumerPlugin, ()) {
                Ok(fid) => {
                    let fiber = reg.get_fiber(fid).expect("registered fiber tracked");
                    fiber.declare_inject::<MtI1>();
                    fiber.declare_inject::<MtU1>();
                    // Delta #1: declarations land outside the state machine,
                    // so reconcile immediately to preserve the invariant.
                    fiber.refresh(&ctx).await;
                    fibers.push((format!("consumer-{fid}"), fiber));
                    newest = Some(fibers.len() - 1);
                }
                Err(e) => {
                    if !e.to_string().contains("duplicate provider") {
                        return Err(format!("step {step}: unexpected register error: {e}"));
                    }
                }
            },
            6 => match ctx.remove::<MtU1>() {
                Ok(_) => {}
                Err(e) => {
                    if !e.to_string().contains("guarded withdrawal") {
                        return Err(format!("step {step}: unexpected removal error: {e}"));
                    }
                }
            },
            _ => {
                if let Some(idx) = newest.take() {
                    fibers[idx].1.dispose().await;
                }
            }
        }
        drain_spawned().await;
        assert_quiescent(&fibers, &ctx)
            .map_err(|e| format!("quiescence broken after step {step}: {e}"))?;
    }

    // Forced convergence: guarantee providers exist, notify reactively, then
    // require the consumer to be Active, epoch-fresh, and store-consistent.
    if ctx.get::<MtI1>().is_none() {
        let fid = reg
            .register(&ctx, MtIndepPlugin, ())
            .map_err(|e| format!("convergence register failed: {e}"))?;
        fibers.push((format!("indep-{fid}"), reg.get_fiber(fid).unwrap()));
    }
    let consumer_idx = match fibers.iter().rposition(|(n, _)| n.starts_with("consumer")) {
        Some(i) => i,
        None => {
            let fid = reg
                .register(&ctx, MtConsumerPlugin, ())
                .map_err(|e| format!("convergence register failed: {e}"))?;
            let fiber = reg.get_fiber(fid).unwrap();
            fiber.declare_inject::<MtI1>();
            fiber.declare_inject::<MtU1>();
            fiber.refresh(&ctx).await;
            fibers.push((format!("consumer-{fid}"), fiber));
            fibers.len() - 1
        }
    };
    if ctx.get::<MtU1>().is_none() {
        ctx.provide(MtU1(4242));
    }
    reflect.notify_with_ctx(TypeId::of::<MtI1>(), &ctx).await;
    reflect.notify_with_ctx(TypeId::of::<MtU1>(), &ctx).await;
    drain_spawned().await;
    assert_quiescent(&fibers, &ctx).map_err(|e| format!("final quiescence: {e}"))?;

    let consumer = fibers[consumer_idx].1.clone();
    consumer.refresh(&ctx).await;
    match consumer.state() {
        FiberState::Active { .. } => {}
        other => return Err(format!("consumer did not converge to Active: {other:?}")),
    }
    let fresh = consumer.compute_epoch(&ctx);
    if consumer.epoch() != fresh {
        return Err(format!(
            "stale epoch '{}' != freshly computed '{}'",
            consumer.epoch(),
            fresh
        ));
    }
    let cval = ctx
        .get::<MtCVal>()
        .ok_or("consumer projection missing after convergence")?;
    let u1 = ctx.get::<MtU1>().ok_or("MtU1 missing after convergence")?;
    if !cval.ready || cval.i1 != 7 || cval.u1 != u1.0 {
        return Err(format!("consumer view diverged from store: {cval:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property 2 — Cor 21 / Thm 73 (registration-order confluence)
// ---------------------------------------------------------------------------

/// Two independent providers and one consumer registering on both.
///
/// Root A registers providers-then-consumer; root B registers consumer-then-
/// providers on a completely fresh context. Both roots converge to the
/// consumer being `Active` with the *identical* epoch string (`compute_epoch`
/// sorts fragments and both sides carry the same provider versions) and the
/// identical projected `MtPair` values. Root B additionally documents the
/// intermediate transition map: consumer `Inactive` before any provider,
/// still `Inactive` with only one provider present, `Active` after both.
pub async fn order_confluence_of_registrations() -> Result<(), String> {
    // Order A: P1, P2, C.
    let (ca, ra, _fa) = base_root();
    ra.register(&ca, MtP1Plugin, ())
        .map_err(|e| format!("A/P1: {e}"))?;
    ra.register(&ca, MtP2Plugin, ())
        .map_err(|e| format!("A/P2: {e}"))?;
    let fid_a = ra
        .register(&ca, MtPairPlugin, ())
        .map_err(|e| format!("A/C: {e}"))?;
    let fib_a = ra.get_fiber(fid_a).expect("tracked");
    fib_a.declare_inject::<MtP1>();
    fib_a.declare_inject::<MtP2>();
    fib_a.refresh(&ca).await;
    drain_spawned().await;
    match fib_a.state() {
        FiberState::Active { .. } => {}
        other => return Err(format!("A: consumer not Active: {other:?}")),
    }
    let epoch_a = fib_a.epoch();
    let pair_a = ca.get::<MtPair>().ok_or("A: pair value missing")?;
    if !(pair_a.ready && pair_a.a == 7 && pair_a.b == 11) {
        return Err(format!("A: wrong projection {pair_a:?}"));
    }

    // Order B: C, P1, P2.
    let (cb, rb, fb) = base_root();
    let fid_b = rb
        .register(&cb, MtPairPlugin, ())
        .map_err(|e| format!("B/C: {e}"))?;
    let fib_b = rb.get_fiber(fid_b).expect("tracked");
    fib_b.declare_inject::<MtP1>();
    fib_b.declare_inject::<MtP2>();
    fib_b.refresh(&cb).await;
    match fib_b.state() {
        FiberState::Inactive { .. } => {}
        other => {
            return Err(format!(
                "B: consumer should be Inactive pre-providers: {other:?}"
            ))
        }
    }
    rb.register(&cb, MtP1Plugin, ())
        .map_err(|e| format!("B/P1: {e}"))?;
    fb.notify_with_ctx(TypeId::of::<MtP1>(), &cb).await;
    drain_spawned().await;
    match fib_b.state() {
        FiberState::Inactive { .. } => {}
        other => {
            return Err(format!(
                "B: consumer should stay Inactive with one provider: {other:?}"
            ))
        }
    }
    rb.register(&cb, MtP2Plugin, ())
        .map_err(|e| format!("B/P2: {e}"))?;
    fb.notify_with_ctx(TypeId::of::<MtP2>(), &cb).await;
    drain_spawned().await;
    match fib_b.state() {
        FiberState::Active { .. } => {}
        other => {
            return Err(format!(
                "B: consumer not Active after both providers: {other:?}"
            ))
        }
    }
    let epoch_b = fib_b.epoch();
    let pair_b = cb.get::<MtPair>().ok_or("B: pair value missing")?;

    if epoch_a != epoch_b {
        return Err(format!(
            "confluence violated: epoch A '{epoch_a}' != epoch B '{epoch_b}'"
        ));
    }
    if pair_b.a != pair_a.a || pair_b.b != pair_a.b || pair_b.ready != pair_a.ready {
        return Err(format!(
            "confluence violated: projections differ ({:?} vs {:?})",
            *pair_a, *pair_b
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property 3 — spatial reactive invariant (dependent never active sans provider)
// ---------------------------------------------------------------------------

/// Verified transition map for a dependent fiber:
///
/// ```text
/// registry path (total factory, check()-gated):
///   Inactive --provide--> Active --provider swap--> Active (new epoch)
///   Active --guarded remove--> refused ("guarded withdrawal")
///   Active --fiber.dispose--> Inactive, projection removed, remove permitted
/// raw-fiber path (kernel primitive):
///   Inactive{error:None} --refresh--> Inactive{"missing or inactive dependency"}
///      --provide+refresh--> Active --provider removal+refresh--> Inactive
/// ```
///
/// Delta #2: a registration whose factory *errors* (rather than returning a
/// `check()==false` value) surfaces `Failed` and skips reflective wiring —
/// see the module docs. These checks use the paper-conform gated factory.
pub async fn dependent_never_active_without_provider() -> Result<(), String> {
    let (ctx, reg, reflect) = base_root();

    // Registry path: registered before the provider exists → Inactive.
    let fid = reg
        .register(&ctx, MtDependentPlugin, ())
        .map_err(|e| format!("register dependent: {e}"))?;
    let dep = reg.get_fiber(fid).ok_or("dependent fiber not tracked")?;
    // Declare the dependency the factory reads at apply-time; declarations
    // land outside the state machine (delta #1), so reconcile eagerly. The
    // declaration also wires ReflectService::dependents for reactive driving.
    dep.declare_inject::<MtProv>();
    dep.refresh(&ctx).await;
    match dep.state() {
        FiberState::Inactive { .. } => {}
        other => return Err(format!("pre-provider state should be Inactive: {other:?}")),
    }

    // Raw-fiber path: declared inject, no refresh yet → pristine Inactive;
    // after an explicit refresh → Inactive with the missing-dependency note.
    let raw = Arc::new(Fiber::new());
    raw.declare_inject::<MtProv>();
    match raw.state() {
        FiberState::Inactive { error: None } => {}
        other => {
            return Err(format!(
                "fresh raw fiber should be Inactive{{error:None}}: {other:?}"
            ))
        }
    }
    raw.refresh(&ctx).await;
    match raw.state() {
        FiberState::Inactive { error: Some(_) } => {}
        other => {
            return Err(format!(
                "refreshed raw fiber should report the missing dep: {other:?}"
            ))
        }
    }

    // Provide v1 → reactive activation of the registry-path dependent.
    ctx.provide(MtProv(1));
    reflect.notify_with_ctx(TypeId::of::<MtProv>(), &ctx).await;
    drain_spawned().await;
    match dep.state() {
        FiberState::Active { .. } => {}
        other => return Err(format!("dependent should activate on provide: {other:?}")),
    }
    let d1 = ctx.get::<MtDerived>().ok_or("derived value missing")?;
    if d1.src != 1 {
        return Err(format!("projection should see v1, got {}", d1.src));
    }
    let epoch_v1 = dep.epoch();
    raw.refresh(&ctx).await;
    if !matches!(raw.state(), FiberState::Active { .. }) {
        return Err(format!("raw fiber should activate: {:?}", raw.state()));
    }

    // Provider swap: same TypeId re-provided → version bump → new epoch and
    // the dependent's projection observes the new value.
    ctx.provide(MtProv(2));
    reflect.notify_with_ctx(TypeId::of::<MtProv>(), &ctx).await;
    drain_spawned().await;
    match dep.state() {
        FiberState::Active { .. } => {}
        other => {
            return Err(format!(
                "dependent should survive provider swap Active: {other:?}"
            ))
        }
    }
    let d2 = ctx
        .get::<MtDerived>()
        .ok_or("derived value missing after swap")?;
    if d2.src != 2 {
        return Err(format!("projection should see v2, got {}", d2.src));
    }
    let epoch_v2 = dep.epoch();
    if epoch_v1 == epoch_v2 {
        return Err(format!(
            "epoch must change across a provider swap (both were '{epoch_v1}')"
        ));
    }

    // Guarded withdrawal: an Active consumer blocks plain removal.
    let err = match ctx.remove::<MtProv>() {
        Err(e) => e,
        Ok(_) => {
            return Err("guarded withdrawal must refuse removal of a consumed provider".to_string())
        }
    };
    if !err.to_string().contains("guarded withdrawal") {
        return Err(format!("unexpected refusal reason: {err}"));
    }

    // Disposal of the consumer lifts the guard and removes the projection.
    dep.dispose().await;
    if ctx.get::<MtDerived>().is_some() {
        return Err("disposal must retract the dependent's projection".to_string());
    }
    let removed = ctx
        .remove::<MtProv>()
        .map_err(|e| format!("removal after disposal should pass: {e}"))?;
    if removed.as_ref().map(|v| v.0) != Some(2) {
        return Err("removed provider should be the v2 instance".to_string());
    }
    reflect.notify_with_ctx(TypeId::of::<MtProv>(), &ctx).await;
    drain_spawned().await;
    raw.refresh(&ctx).await;
    match raw.state() {
        FiberState::Inactive { .. } => {}
        other => {
            return Err(format!(
                "raw fiber must deactivate after provider removal: {other:?}"
            ))
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property 4 — Thm 16 (LIFO disposal restores the store)
// ---------------------------------------------------------------------------

/// Direct provides and plugin-installed disposables unwind in strict reverse
/// registration order, leaving the store byte-for-byte restored.
///
/// Undo-stack layout after `Mark1, Mark2, EffA, EffB` (bottom → top):
/// `[uM1, uM2, uProvA, dispA, uProvB, dispB]`; popping must run
/// `dispB("B"), undo ProvB, dispA("A"), undo ProvA, undo M2, undo M1`.
pub async fn lifo_dispose_restores_store() -> Result<(), String> {
    let ctx = Context::new_root();
    let pre = ctx.snapshot_len();
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let count = Arc::new(AtomicUsize::new(0));

    ctx.provide(MtMark1(1));
    ctx.provide(MtMark2(2));
    ctx.plugin(MtEffA {
        log: log.clone(),
        count: count.clone(),
    })
    .await
    .map_err(|e| format!("plugin A: {e}"))?;
    ctx.plugin(MtEffB {
        log: log.clone(),
        count: count.clone(),
    })
    .await
    .map_err(|e| format!("plugin B: {e}"))?;

    if !matches!(ctx.fiber().state(), FiberState::Active { .. }) {
        return Err(format!(
            "root fiber should be Active after plugins: {:?}",
            ctx.fiber().state()
        ));
    }

    ctx.fiber().dispose().await;

    if count.load(Ordering::SeqCst) != 2 {
        return Err(format!(
            "each disposable must run exactly once, counter={}",
            count.load(Ordering::SeqCst)
        ));
    }
    let order = log.lock().clone();
    if order != ["B", "A"] {
        return Err(format!(
            "LIFO order violated: {order:?} (expected [\"B\", \"A\"])"
        ));
    }
    if ctx.get::<MtMark1>().is_some()
        || ctx.get::<MtMark2>().is_some()
        || ctx.get::<MtEffA>().is_some()
        || ctx.get::<MtEffB>().is_some()
    {
        return Err("store not fully restored after disposal".to_string());
    }
    if ctx.snapshot_len() != pre {
        return Err(format!(
            "store length {} after disposal, expected {pre}",
            ctx.snapshot_len()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CI wrappers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn metatheory_quiescence_after_every_op() {
        quiescence_after_every_op()
            .await
            .expect("Thm 66 quiescence property");
    }

    #[tokio::test]
    async fn metatheory_order_confluence_of_registrations() {
        order_confluence_of_registrations()
            .await
            .expect("Cor 21 / Thm 73 confluence property");
    }

    #[tokio::test]
    async fn metatheory_dependent_never_active_without_provider() {
        dependent_never_active_without_provider()
            .await
            .expect("spatial reactive invariant");
    }

    #[tokio::test]
    async fn metatheory_lifo_dispose_restores_store() {
        lifo_dispose_restores_store()
            .await
            .expect("Thm 16 LIFO disposal property");
    }
}
