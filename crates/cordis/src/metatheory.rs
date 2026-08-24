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
//!   consumers. Legs D–H cover the two former deltas: failed factories stay
//!   inspectable (`Failed` terminal-rest) with notified dependents and fresh-id
//!   re-registration supersession, and late inject declarations on Active
//!   fibers reconcile immediately.
//! * [`lifo_dispose_restores_store`] — Thm 16: disposal unwinds registration
//!   effects in strict LIFO order and restores the store to its pre-
//!   registration contents.
//!
//! # Former deltas between paper and implementation (resolved)
//!
//! 1. `Fiber::declare_inject` used to mutate the inject set *outside* the
//!    fiber state machine, letting an `Active` fiber carry a freshly
//!    declared (unsatisfied) inject until an external driver refreshed it.
//!    Resolved: a declaration landing on a fiber resting `Active` now
//!    reconciles immediately (`Fiber::reconcile_after_declare`) with the
//!    same transition shape `refresh` uses — satisfied declarations update
//!    the epoch in place, unsatisfied ones undo effects and rest `Inactive`
//!    with the missing-dependency note. A declaration racing an in-flight
//!    refresh is folded into that refresh's recompute loop via a pending
//!    flag instead of being lost between passes. Declarations on
//!    `Inactive`/`Failed` fibers keep their historical behavior: they are
//!    evaluated at the fiber's next transition.
//! 2. A registration whose factory **errors** used to surface `Failed` and
//!    skip all reflective wiring — the fiber was unreachable, undriven by
//!    notifications, and its provider key stayed invisible to dependents.
//!    Resolved: `RegistryService::register` still returns `Err`, but the
//!    Failed fiber now enters the bookkeeping graph
//!    (`RegistryService::wire_failed_registration`): it stays inspectable
//!    via `get_fiber` under its fresh id, is registered with
//!    `ReflectService` against the attempted key, and a notify fans out so
//!    dependents observe the provider loss reactively. The failed fiber
//!    rests in terminal `Failed{error}` at quiescence (a rest state), while
//!    its *dependents* rest `Inactive` — the paper-conform outcome. A later
//!    successful registration of the same key allocates a fresh fiber id
//!    (the failed one never occupied the provided slot) and reactivates the
//!    dependents against the new instance. See
//!    [`dependent_never_active_without_provider`] legs D/E/F/G/H.
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
use crate::{FiberId, ReflectService};

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

/// Factory whose `apply` always errors (scenario 3, failed-registration leg).
struct MtFailPlugin;
impl Plugin for MtFailPlugin {
    type Config = ();
    type Provides = MtProv;
    fn apply(&self, _ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtProv>, CordisError> {
        Err(CordisError::Configuration("factory exploded".into()))
    }
}

/// Successful replacement factory for the key `MtFailPlugin` failed on
/// (scenario 3, re-registration supersession leg).
struct MtRevivePlugin;
impl Plugin for MtRevivePlugin {
    type Config = ();
    type Provides = MtProv;
    fn apply(&self, _ctx: &Arc<Context>, _config: ()) -> Result<Arc<MtProv>, CordisError> {
        Ok(Arc::new(MtProv(9)))
    }
}

/// Service type never provided anywhere (scenario 3, unsatisfied-declaration
/// leg of the eager-reconcile check).
#[derive(Debug)]
struct MissingProbe;
impl Service for MissingProbe {}

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
/// handing out a value. `Failed{error}` is a terminal *rest* state (former
/// delta #2, resolved): failed fibers are tracked bookkeeping, so they are
/// allowed at rest.
fn assert_quiescent(fibers: &[(String, Arc<Fiber>)], ctx: &Arc<Context>) -> Result<(), String> {
    for (name, fiber) in fibers {
        match fiber.state() {
            FiberState::Loading | FiberState::Reloading | FiberState::Unloading { .. } => {
                return Err(format!(
                    "fiber '{name}' rests in transitional state {:?}",
                    fiber.state()
                ));
            }
            // Failed is terminal-rest; see the module docs.
            FiberState::Failed { .. } | FiberState::Inactive { .. } => {}
            FiberState::Active { .. } => {
                for tid in fiber.injected_type_ids() {
                    if !ctx.is_available(tid) {
                        return Err(format!(
                            "fiber '{name}' is Active but injected dependency {tid:?} is unavailable"
                        ));
                    }
                }
            }
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
                    // Declarations on this freshly registered (Inactive)
                    // fiber evaluate at its next transition; the explicit
                    // refresh here reconciles immediately.
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
/// failed-registration path (former delta #2, resolved):
///   failing factory --> register Err + fiber Failed{error}, inspectable,
///      dependents notified; dependent rests Inactive at quiescence
///   successful re-register of same key --> fresh fiber id, dependents Active
/// late-declaration path (former delta #1, resolved):
///   Active --declare_inject(satisfied)--> Active (epoch folds the inject)
///   Active --declare_inject(unsatisfied)--> Reloading -> Inactive{missing dep}
/// ```
///
/// The failed-registration and late-declaration legs cover the two former
/// documented deltas; see the module docs.
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

    // --- Leg D (former delta #2): failing factory is inspectable as Failed
    // and its provider key loss is observed by dependents.
    let fail_err = reg
        .register(&ctx, MtFailPlugin, ())
        .expect_err("failing factory must be refused");
    if !fail_err.to_string().contains("factory exploded") {
        return Err(format!("unexpected failure reason: {fail_err}"));
    }
    let fail_fid = next_fid_after(&reg, fid)?;
    let failed = reg
        .get_fiber(fail_fid)
        .ok_or("failed fiber must stay inspectable via get_fiber")?;
    match failed.state() {
        FiberState::Failed { error } => {
            if !error.as_deref().unwrap_or("").contains("factory exploded") {
                return Err("Failed state must carry the factory error".to_string());
            }
        }
        other => return Err(format!("expected Failed{{error}}, got {other:?}")),
    }
    // No phantom instance materialized for the failed key.
    if ctx.get::<MtProv>().is_some() {
        return Err("failed factory must not provide an instance".to_string());
    }
    // Dependent was notified through the same path successful registrations
    // use: it re-evaluated against a still-missing key and stays Inactive.
    match dep.state() {
        FiberState::Inactive { .. } => {}
        other => {
            return Err(format!(
                "dependent must rest Inactive while its key has no live provider: {other:?}"
            ))
        }
    }

    // --- Leg E: the provided slot stayed vacant — a duplicate registration
    // of the SAME key is not refused.
    let revive_fid = reg
        .register(&ctx, MtRevivePlugin, ())
        .map_err(|e| format!("re-register after failure must succeed: {e}"))?;
    if revive_fid == fail_fid {
        return Err("re-registration must allocate a fresh fiber id".to_string());
    }
    let revived = reg
        .get_fiber(revive_fid)
        .ok_or("revived fiber must be tracked")?;
    match revived.state() {
        FiberState::Active { .. } => {}
        other => return Err(format!("revived registration should be Active: {other:?}")),
    }
    // The failed fiber keeps its terminal Failed state (fresh-name rule).
    if !matches!(
        reg.get_fiber(fail_fid)
            .ok_or("failed fiber vanished")?
            .state(),
        FiberState::Failed { .. }
    ) {
        return Err("failed fiber must remain terminal Failed".to_string());
    }

    // --- Leg F: the revived provider feeds a fresh dependent through the
    // normal reactive path, and former delta #1 shows up twice: the healthy
    // gated factory is Active the moment it registers, and the subsequent
    // inject declaration reconciles EAGERLY, folding the provider into the
    // epoch without any external refresh.
    let late_fid = reg
        .register(&ctx, MtDependentPlugin, ())
        .map_err(|e| format!("dependent registration on revived key failed: {e}"))?;
    let late = reg
        .get_fiber(late_fid)
        .ok_or("late dependent fiber not tracked")?;
    match late.state() {
        FiberState::Active { .. } if late.epoch() == ":" => {}
        other => {
            return Err(format!(
                "healthy dependent over a present provider must register Active \
                 with an inject-less epoch: {other:?}"
            ))
        }
    }
    late.declare_inject::<MtProv>();
    match late.state() {
        FiberState::Active { .. } => {}
        other => {
            return Err(format!(
                "satisfied declaration must keep the fiber Active eagerly: {other:?}"
            ))
        }
    }
    if !late.epoch().contains("MtProv") {
        return Err(format!(
            "declaration must fold the provider into the epoch eagerly: {}",
            late.epoch()
        ));
    }
    let d3 = ctx
        .get::<MtDerived>()
        .ok_or("derived value missing after revival")?;
    if d3.src != 9 {
        return Err(format!(
            "projection should observe revived v9, got {}",
            d3.src
        ));
    }

    // --- Leg G (former delta #1): a satisfied declaration landing on this
    // resting-Active fiber reconciles immediately — epoch folds the inject,
    // no external refresh needed.
    let before = revived.epoch();
    revived.declare_inject::<MtProv>();
    let after = revived.epoch();
    if !matches!(revived.state(), FiberState::Active { .. }) {
        return Err(format!(
            "satisfied declaration must keep the fiber Active: {:?}",
            revived.state()
        ));
    }
    if after == before {
        return Err("satisfied declaration must fold into the epoch immediately".to_string());
    }

    // --- Leg H (former delta #1): an unsatisfied declaration on the same
    // Active fiber drives it Inactive right away, with effects undone.
    revived.declare_inject::<MissingProbe>();
    match revived.state() {
        FiberState::Inactive { error: Some(note) } => {
            if !note.contains("missing or inactive dependency") {
                return Err(format!("unexpected deactivation note: {note}"));
            }
        }
        other => {
            return Err(format!(
                "unsatisfied declaration must deactivate the fiber eagerly: {other:?}"
            ))
        }
    }
    // The provider withdrawal succeeded because the consumer had already
    // left the Active set — the store really lost the projection.
    if ctx.get::<MtProv>().is_some() {
        return Err("deactivated fiber's projection must be withdrawn".to_string());
    }
    Ok(())
}

/// The registry's `next_id` counter is private; derive the fresh id of the
/// most recent registration from the tracked population instead.
fn next_fid_after(reg: &RegistryService, prev_max: FiberId) -> Result<FiberId, String> {
    // Walk upward from prev_max: ids are dense and monotonic per registry.
    for fid in (prev_max + 1)..=(prev_max + 64) {
        if reg.get_fiber(fid).is_some() {
            return Ok(fid);
        }
    }
    Err("no tracked fiber found above the previous max id".to_string())
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

#[cfg(test)]
mod version_conformance {
    use super::*;

    /// Versioned provider service for the version-conformance checks.
    #[derive(Debug)]
    struct VcPeer(pub u64);
    impl Service for VcPeer {}

    /// Dependent service built from a versioned peer; `check()` reports whether
    /// the observed peer value carries the expected major.
    #[derive(Debug)]
    struct VcDependent {
        peer: Option<u64>,
    }
    impl Service for VcDependent {
        fn check(&self) -> bool {
            self.peer.is_some()
        }
    }

    /// Consumer plugin that reads the peer through the context at apply time.
    struct VcConsumerPlugin;
    impl Plugin for VcConsumerPlugin {
        type Config = ();
        type Provides = VcDependent;
        fn apply(&self, ctx: &Arc<Context>, _config: ()) -> Result<Arc<VcDependent>, CordisError> {
            Ok(Arc::new(VcDependent {
                peer: ctx.get::<VcPeer>().map(|p| p.0),
            }))
        }
    }

    /// Requirement encoding helper mirroring the documented scheme:
    /// `major * VERSION_MAJOR_SCALE + floor`.
    fn requirement(major: u64, floor: u64) -> u64 {
        major * crate::Context::VERSION_MAJOR_SCALE + floor
    }

    /// A versioned provide satisfies an inject whose constraint shares the
    /// provider's major and sits at or below the provider version; the
    /// dependent activates and its projection observes the provider value.
    #[tokio::test]
    async fn versioned_provide_satisfies_compatible_constraint() {
        let (ctx, reg, _reflect) = base_root();
        ctx.provide_versioned(VcPeer(100_001), 100_001);

        let fid = reg
            .register(&ctx, VcConsumerPlugin, ())
            .expect("consumer registration");
        let fiber = reg.get_fiber(fid).expect("tracked consumer");
        fiber.declare_inject_versioned::<VcPeer>(Some(requirement(1, 1)));
        fiber.refresh(&ctx).await;

        match fiber.state() {
            FiberState::Active { .. } => {}
            other => panic!("compatible provider must activate the consumer, got {other:?}"),
        }
        let dep = ctx.get::<VcDependent>().expect("projection provided");
        assert_eq!(dep.peer, Some(100_001));
    }

    /// A cross-major or below-floor provider keeps the dependent Inactive,
    /// never silently binding; re-registering a compatible same-major
    /// provider reactively flips it Active through the notify-driven refresh
    /// path.
    #[tokio::test]
    async fn version_mismatch_keeps_dependent_inactive_until_compatible_upgrade() {
        let (ctx, reg, reflect) = base_root();
        ctx.provide_versioned(VcPeer(200_001), 200_001);

        let fid = reg
            .register(&ctx, VcConsumerPlugin, ())
            .expect("consumer registration");
        let fiber = reg.get_fiber(fid).expect("tracked consumer");
        fiber.declare_inject_versioned::<VcPeer>(Some(requirement(1, 1)));
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Inactive { .. }));
        assert!(ctx.get::<VcDependent>().is_none());

        // Reactive upgrade path: replacing the provider with a compatible
        // same-major version notifies dependents and flips them Active.
        let old = ctx
            .remove::<VcPeer>()
            .expect("withdrawal allowed while consumer is Inactive")
            .expect("provider present");
        assert_eq!(old.0, 200_001);
        ctx.provide_versioned(VcPeer(100_005), 100_005);
        reflect.notify_with_ctx(TypeId::of::<VcPeer>(), &ctx).await;
        drain_spawned().await;

        match fiber.state() {
            FiberState::Active { .. } => {}
            other => panic!("compatible upgrade must reactivate the consumer, got {other:?}"),
        }
        let dep = ctx.get::<VcDependent>().expect("projection re-provided");
        assert_eq!(dep.peer, Some(100_005));
    }

    /// Legacy unversioned provides carry semantic version 0: they satisfy
    /// unconstrained injects exactly as before but are rejected by any
    /// positive-major requirement — the documented migration story.
    #[tokio::test]
    async fn legacy_provide_defaults_zero_and_satisfies_unconstrained_only() {
        let (ctx, reg, _reflect) = base_root();
        ctx.provide(VcPeer(42));

        let fid = reg
            .register(&ctx, VcConsumerPlugin, ())
            .expect("consumer registration");
        let fiber = reg.get_fiber(fid).expect("tracked consumer");
        fiber.declare_inject_versioned::<VcPeer>(None);
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Active { .. }));
        assert_eq!(ctx.get::<VcDependent>().unwrap().peer, Some(42));

        // Same legacy provider fails a major-1 requirement.
        fiber.declare_inject_versioned::<VcPeer>(Some(requirement(1, 0)));
        fiber.refresh(&ctx).await;
        assert!(matches!(fiber.state(), FiberState::Inactive { .. }));

        // And the semantic version really is 0 on the unversioned path.
        assert_eq!(ctx.provider_version(TypeId::of::<VcPeer>()), 0);
    }
}
