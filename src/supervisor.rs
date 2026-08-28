//! Daemon-side half of the supervised-worker protocol.
//!
//! A server started under [`supervise`] does its real work in a child copy
//! of the same executable. The child carries the [`CHILD_ENV_MARKER`]
//! environment variable, so the worker-side stdin watcher inside the child
//! knows it is supervised and can watch standard input to detect daemon
//! death.
//!
//! Protocol summary:
//!
//! - Exit code [`EXIT_RESTART`] asks the loop to start a fresh child.
//! - Exit code [`EXIT_QUIT`] and every other terminal status end the loop.
//! - Exit code [`EXIT_BOOT`] reports a failed boot. The loop treats it like
//!   any normal terminal status and returns `Ok`; the caller mirrors the
//!   real child exit code to its own process exit, so the service manager
//!   still sees the non-zero failure.
//!
//! [`spawn_self_supervised`] creates the child and hands back the write end
//! of its standard input. Dropping that handle closes the pipe, the child's
//! standard input reaches end-of-file, and the worker-side watcher performs
//! a graceful teardown.
//!
//! Two safeguards keep the daemon responsive. Respawns after runs too short
//! to prove health pace themselves exponentially (`next_backoff`: 100 ms
//! doubling to a 5 s cap) instead of hammering at full speed, and a worker
//! that has been asked to stop but ignores the request is force-killed once
//! [`WORKER_SHUTDOWN_GRACE`] elapses ([`wait_with_grace`]).

use std::future::Future;
use std::io;
use std::time::Duration;

/// Environment variable that marks a supervised child process.
///
/// Same literal as the worker-side `SUPERVISED_ENV` constant. Presence
/// alone matters; the value is ignored.
pub const CHILD_ENV_MARKER: &str = "CORDIS_SUPERVISED";

/// Child exit code: restart me with a fresh process.
pub const EXIT_RESTART: i32 = 51;

/// Child exit code: shut down for good.
pub const EXIT_QUIT: i32 = 52;

/// Child exit code: boot failed; do not restart.
pub const EXIT_BOOT: i32 = 53;

/// Time window checked by the rapid-restart guard.
const RAPID_RESTART_WINDOW: Duration = Duration::from_secs(30);

/// Number of restarts inside [`RAPID_RESTART_WINDOW`] that stops the loop.
const RAPID_RESTART_LIMIT: usize = 5;

/// A child run that outlived this duration before exiting counts as a
/// healthy cadence: the restart ladder resets so the next crash sequence
/// starts from zero instead of inheriting stale strikes.
const HEALTHY_RUN_DURATION: Duration = Duration::from_secs(10 * 60);

/// A child run that exits sooner than this never proved the worker could
/// serve, so it counts as unhealthy: the next respawn is delayed by
/// `next_backoff`.
const UNHEALTHY_RUN_DURATION: Duration = Duration::from_secs(10);

/// Delay before the first respawn that follows an unhealthy run; each
/// further consecutive unhealthy run doubles it, up to `BACKOFF_MAX_DELAY`.
const BACKOFF_INITIAL_DELAY: Duration = Duration::from_millis(100);

/// Upper bound for the exponentially growing respawn delay.
const BACKOFF_MAX_DELAY: Duration = Duration::from_secs(5);

/// Grace window granted to a worker that has been asked to stop (its
/// standard input reached end-of-file) before the daemon force-kills it.
/// Bounds the goodbye, never the working lifetime.
pub const WORKER_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Waits for `child` to exit, granting it [`WORKER_SHUTDOWN_GRACE`] once its
/// standard input has been dropped (the stop request). A worker that exits
/// within the window yields its real code; one that outstays the grace is
/// force-killed and the resulting status supplies the code instead, so a
/// hung child can never park the daemon forever.
///
/// Callers must already have released the child's stdin handle: the grace
/// bounds the goodbye, not the working lifetime.
pub async fn wait_with_grace(child: &mut tokio::process::Child) -> Option<i32> {
    match tokio::time::timeout(WORKER_SHUTDOWN_GRACE, child.wait()).await {
        Ok(Ok(status)) => status.code(),
        // Wait error (polling failure): nothing more to reap.
        Ok(Err(_)) => None,
        // Grace elapsed while the child ignored EOF: kill it and take the
        // code from the forced-exit status.
        Err(_) => {
            tracing::warn!("worker exceeded shutdown grace, killed");
            let _ = child.kill().await;
            child.wait().await.ok().and_then(|s| s.code())
        }
    }
}

/// Respawn delay after `consecutive_unhealthy` runs that each exited before
/// [`UNHEALTHY_RUN_DURATION`]: 100 ms doubling per strike, capped at 5 s —
/// 100 ms, 200 ms, 400 ms, 800 ms, 1.6 s, 3.2 s, 5 s, 5 s, ...
fn next_backoff(consecutive_unhealthy: u32) -> Duration {
    let shift = consecutive_unhealthy.min(16);
    BACKOFF_INITIAL_DELAY
        .checked_mul(1u32 << shift)
        .unwrap_or(BACKOFF_MAX_DELAY)
        .min(BACKOFF_MAX_DELAY)
}

/// Wall-clock milliseconds since the Unix epoch; the loop's single time
/// source. Tests override it via [`NOW_OVERRIDE`] to simulate long-lived
/// children without sleeping.
fn now() -> u64 {
    #[cfg(test)]
    if let Some(ms) = *NOW_OVERRIDE.lock() {
        return ms;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
static NOW_OVERRIDE: parking_lot::Mutex<Option<u64>> = parking_lot::Mutex::new(None);

/// Respawn delays requested by [`supervise`], recorded so tests assert the
/// pacing instead of measuring slept wall-clock time. Guarded by the tests'
/// `ENV_LOCK`: every `supervise` caller holds it, so mutations serialise.
#[cfg(test)]
static BACKOFF_DELAYS: parking_lot::Mutex<Vec<Duration>> = parking_lot::Mutex::new(Vec::new());

/// A running child together with the write end of its standard input.
///
/// # Lifetime contract
///
/// Hold [`stdin`](SupervisedChild::stdin) for as long as the child should
/// live. Dropping it closes the pipe: the child sees end-of-file on its
/// standard input, and the worker-side watcher tears the child down
/// gracefully. The drop alone does not kill the child; the shutdown path
/// relies on the pipe close.
pub struct SupervisedChild {
    /// The running child process.
    pub child: std::process::Child,
    /// Write end of the child's standard input pipe.
    pub stdin: std::process::ChildStdin,
}

/// Returns true when this process itself runs as a supervised child.
///
/// Children never self-supervise; see [`supervise`].
pub fn is_supervised() -> bool {
    std::env::var_os(CHILD_ENV_MARKER).is_some()
}

/// Runs the restart loop around `run_child`.
///
/// `run_child` starts one child run and yields its exit code as
/// `Option<i32>`: `Some(code)` for a known code, `None` when the status
/// carries no code (death by signal, for example). Translate
/// [`std::process::ExitStatus`] with [`std::process::ExitStatus::code`] and
/// pass its result straight through; `None` behaves like a normal terminal
/// status and ends the loop.
///
/// Behaviour:
///
/// - When this process is already a supervised child, nested supervision is
///   refused: the function returns `Ok` at once and never calls
///   `run_child`.
/// - [`EXIT_RESTART`] respawns the child.
/// - Every other outcome ends the loop with `Ok`: [`EXIT_QUIT`],
///   [`EXIT_BOOT`], any other code, and unknown statuses. [`EXIT_BOOT`]
///   therefore surfaces as an ordinary return; the caller should exit with
///   the child's real code so the service manager observes the failure.
/// - [`RAPID_RESTART_LIMIT`] restarts packed inside
///   [`RAPID_RESTART_WINDOW`] (a plugin that crashes at boot, for example)
///   stop the loop with an error instead of spinning.
/// - A child that ran for at least [`HEALTHY_RUN_DURATION`] before exiting
///   clears the accumulated restart ladder first: a long-lived run proves a
///   healthy cadence, so old strikes never doom the fresh process.
/// - Respawn after a run shorter than [`UNHEALTHY_RUN_DURATION`] is delayed
///   by [`next_backoff`] (100 ms doubling per consecutive unhealthy run,
///   capped at 5 s); any run at or beyond the healthy threshold resets the
///   delay to the first step. The pacing counter and the rapid-restart
///   ladder share one health definition.
pub async fn supervise<F, Fut>(run_child: F) -> Result<(), io::Error>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Option<i32>>,
{
    if is_supervised() {
        return Ok(());
    }

    // Restart ladder: wall-clock milliseconds (via [`now`]) of the last
    // respawns. Milliseconds keep the arithmetic testable through the clock
    // seam without `Instant` subtraction.
    let mut restarts: Vec<u64> = Vec::new();
    // When the current child run started; compared against [`now`] at exit
    // to detect a healthy long-lived run.
    let mut spawned_at = now();

    // Consecutive runs that each ended inside [`UNHEALTHY_RUN_DURATION`];
    // drives respawn pacing through [`next_backoff`] until a run proves
    // healthy again.
    let mut consecutive_unhealthy: u32 = 0;

    loop {
        let code = run_child().await;
        let ran_for = now().saturating_sub(spawned_at);
        let healthy_run = ran_for >= UNHEALTHY_RUN_DURATION.as_millis() as u64;
        if healthy_run {
            consecutive_unhealthy = 0;
        }

        match code {
            Some(EXIT_RESTART) => {}
            // EXIT_QUIT, EXIT_BOOT, other codes, and unknown statuses all
            // end the loop. The caller mirrors the child's real exit code,
            // so EXIT_BOOT still reaches the service manager as a failure.
            _ => return Ok(()),
        }

        // A run that outlived [`HEALTHY_RUN_DURATION`] proves the cadence is
        // healthy: stale strikes say nothing about the fresh process, so the
        // ladder starts over.
        if ran_for >= HEALTHY_RUN_DURATION.as_millis() as u64 {
            restarts.clear();
            tracing::info!(
                ran_for_ms = ran_for,
                "supervisor: long-lived worker exited cleanly; restart backoff reset"
            );
        }

        // Pace respawns after unhealthy runs so a crash-loop burns time
        // exponentially instead of respawning at full speed until the
        // rapid-restart cap trips.
        if !healthy_run {
            let delay = next_backoff(consecutive_unhealthy);
            tracing::warn!(
                delay_ms = delay.as_millis() as u64,
                consecutive_unhealthy,
                "supervisor: worker exited before proving health; backing off before respawn"
            );
            #[cfg(test)]
            BACKOFF_DELAYS.lock().push(delay);
            #[cfg(not(test))]
            tokio::time::sleep(delay).await;
            consecutive_unhealthy += 1;
        }

        let stamp = now();
        restarts.retain(|at| stamp.saturating_sub(*at) < RAPID_RESTART_WINDOW.as_millis() as u64);
        restarts.push(stamp);
        if restarts.len() >= RAPID_RESTART_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rapid restart loop detected",
            ));
        }
        spawned_at = now();
    }
}

/// Re-execs the current executable as a supervised child.
///
/// The child inherits every command-line argument after the program name,
/// the whole environment, and the parent's standard output and standard
/// error. Two things change:
///
/// - [`CHILD_ENV_MARKER`] is set, so the child knows it is supervised and
///   its stdin watcher can detect daemon death.
/// - Standard input becomes a pipe whose write end is returned inside the
///   [`SupervisedChild`] handle.
///
/// Drop the [`SupervisedChild::stdin`] handle when the child should stop;
/// see the lifetime contract on [`SupervisedChild`].
pub fn spawn_self_supervised() -> Result<SupervisedChild, io::Error> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .args(std::env::args_os().skip(1))
        .env(CHILD_ENV_MARKER, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "child standard input pipe was not created",
        )
    })?;

    Ok(SupervisedChild { child, stdin })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Serialises every test that touches the process-wide environment.
    ///
    /// A Tokio mutex because the guard is deliberately held across
    /// `supervise(...)` await points.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Status provider for [`supervise`]: serves one queued status per
    /// call and records how often it ran.
    #[derive(Clone)]
    struct Script {
        statuses: Arc<Vec<Option<i32>>>,
        calls: Arc<AtomicUsize>,
    }

    impl Script {
        fn new(statuses: Vec<Option<i32>>) -> Self {
            Self {
                statuses: Arc::new(statuses),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// One child run: hand out the next queued status. Exhausted
        /// scripts report a clean exit so loops cannot spin past the queue.
        async fn step(self) -> Option<i32> {
            let nth = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.statuses.get(nth) {
                Some(code) => *code,
                None => Some(0),
            }
        }
    }

    #[tokio::test]
    async fn child_exit_codes_drive_loop() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        let script = Script::new(vec![Some(EXIT_RESTART), Some(0)]);
        let result = supervise(|| script.clone().step()).await;

        assert!(result.is_ok());
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test]
    async fn rapid_restart_cap_trips() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        let script = Script::new(vec![Some(EXIT_RESTART); RAPID_RESTART_LIMIT]);
        let result = supervise(|| script.clone().step()).await;

        let err = result.expect_err("five rapid restarts must stop the loop");
        assert!(err.to_string().contains("rapid restart loop"));
        assert_eq!(script.calls(), RAPID_RESTART_LIMIT);
    }

    /// Scripted statuses plus scripted exit timestamps, driving the [`now`]
    /// clock seam so child-run durations are simulated without sleeping.
    #[derive(Clone)]
    struct ClockScript {
        statuses: Arc<Vec<Option<i32>>>,
        /// Fake wall-clock milliseconds at which each run exits.
        exits: Arc<Vec<u64>>,
        calls: Arc<AtomicUsize>,
    }

    impl ClockScript {
        fn new(statuses: Vec<Option<i32>>, exits: Vec<u64>) -> Self {
            Self {
                statuses: Arc::new(statuses),
                exits: Arc::new(exits),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        async fn step(self) -> Option<i32> {
            let nth = self.calls.fetch_add(1, Ordering::SeqCst);
            // This run "finishes" at its scripted exit time.
            if let Some(at) = self.exits.get(nth) {
                *NOW_OVERRIDE.lock() = Some(*at);
            }
            match self.statuses.get(nth) {
                Some(code) => *code,
                None => Some(0),
            }
        }
    }

    /// Three sub-second crash loops, then a worker that outlived
    /// [`HEALTHY_RUN_DURATION`]: the accumulated ladder clears, so two more
    /// rapid crashes stay under the cap instead of tripping it.
    #[tokio::test]
    async fn long_lived_run_resets_rapid_restart_ladder() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        const HEALTHY_MS: u64 = HEALTHY_RUN_DURATION.as_millis() as u64;
        const BASE: u64 = 1_000_000_000;

        // Exits: three 1ms-apart crashes, one healthy-length run, then two
        // more rapid crashes. The exhausted script then reports a clean exit,
        // ending the loop.
        let exits = vec![
            BASE + 1,
            BASE + 2,
            BASE + 3,
            BASE + 3 + HEALTHY_MS,
            BASE + 3 + HEALTHY_MS + 4,
        ];
        let script = ClockScript::new(vec![Some(EXIT_RESTART); 5], exits);

        *NOW_OVERRIDE.lock() = Some(BASE);
        let result = supervise(|| script.clone().step()).await;
        *NOW_OVERRIDE.lock() = None;

        assert!(
            result.is_ok(),
            "two post-reset crashes must stay under the cap: {result:?}"
        );
        // Five queued runs plus the final exhausted-script probe.
        assert_eq!(script.calls(), 6);
    }

    /// Counterfactual: the same crash cadence WITHOUT the long-lived run
    /// still trips the cap — the reset, not the clock seam, changed the
    /// outcome.
    #[tokio::test]
    async fn all_rapid_runs_without_reset_still_trip_cap() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        const BASE: u64 = 2_000_000_000;
        let exits: Vec<u64> = (1..=5).map(|i| BASE + i).collect();
        let script = ClockScript::new(vec![Some(EXIT_RESTART); 5], exits);

        *NOW_OVERRIDE.lock() = Some(BASE);
        let result = supervise(|| script.clone().step()).await;
        *NOW_OVERRIDE.lock() = None;

        let err = result.expect_err("five rapid restarts must stop the loop");
        assert!(err.to_string().contains("rapid restart loop"));
        assert_eq!(script.calls(), RAPID_RESTART_LIMIT);
    }

    #[tokio::test]
    async fn supervised_mode_short_circuits() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var(CHILD_ENV_MARKER, "1");
        assert!(is_supervised());

        let script = Script::new(Vec::new());
        let result = supervise(|| script.clone().step()).await;

        assert!(result.is_ok());
        assert_eq!(script.calls(), 0);

        std::env::remove_var(CHILD_ENV_MARKER);
    }

    /// Grace constant is part of the daemon's operational contract: a hung
    /// worker must never hold the daemon past this window.
    #[test]
    fn shutdown_grace_is_ten_seconds() {
        assert_eq!(WORKER_SHUTDOWN_GRACE, Duration::from_secs(10));
    }

    /// A child that exits well inside the grace window yields its real exit
    /// code unchanged — the kill path never fires for cooperative workers.
    #[tokio::test]
    async fn wait_with_grace_returns_fast_child_code() {
        let mut child = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .spawn()
            .expect("spawn true");
        let code = wait_with_grace(&mut child).await;
        assert_eq!(code, Some(0));
    }

    /// A child that ignores EOF outstays the grace: it is force-killed and
    /// the code comes from the forced-exit status (signal death → None).
    /// Uses a short-lived `sleep` child only to prove the timeout branch is
    /// reachable; the 10 s wall cost is bounded by the grace itself.
    #[tokio::test]
    async fn wait_with_grace_kills_child_after_timeout() {
        // `cat` with no input would also hang, but `sleep` ignores nothing
        // we rely on; both work. SIGKILL on Linux yields status.code() ==
        // None, so the observable outcome is the absence of a code plus the
        // fact that this returns instead of hanging forever.
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let code = wait_with_grace(&mut child).await;
        // Killed by SIGKILL: no exit code, but crucially no hang.
        assert_eq!(code, None);
    }

    /// Backoff table: first unhealthy respawn waits 100 ms, each further
    /// consecutive unhealthy run doubles it, capped at 5 s.
    #[test]
    fn backoff_doubles_and_caps() {
        let cases = [
            (0u32, Duration::from_millis(100)),
            (1, Duration::from_millis(200)),
            (2, Duration::from_millis(400)),
            (3, Duration::from_millis(800)),
            (4, Duration::from_millis(1600)),
            (5, Duration::from_millis(3200)),
            (6, Duration::from_secs(5)),
            (7, Duration::from_secs(5)),
            (100, Duration::from_secs(5)),
            (u32::MAX, Duration::from_secs(5)),
        ];
        for (n, expected) in cases {
            assert_eq!(
                next_backoff(n),
                expected,
                "next_backoff({n}) must be {expected:?}"
            );
        }
    }

    /// A crash sequence through the full loop paces its respawns: delays
    /// follow the doubling table while runs stay unhealthy, and one healthy
    /// (HEALTHY_RUN_DURATION-length) run resets the counter so the next
    /// crash starts over at 100 ms — same reset condition as the ladder.
    #[tokio::test]
    async fn healthy_run_resets_backoff_counter() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        const HEALTHY_MS: u64 = HEALTHY_RUN_DURATION.as_millis() as u64;
        const BASE: u64 = 3_000_000_000;

        BACKOFF_DELAYS.lock().clear();
        // Two rapid crashes (delays 100 ms, 200 ms), then a healthy-length
        // run, then another rapid crash whose delay must restart at 100 ms.
        // The exhausted script then reports a clean exit ending the loop.
        const UNHEALTHY_MS: u64 = UNHEALTHY_RUN_DURATION.as_millis() as u64;
        // Run 4 spawns when run 3 exits (clock = BASE+2+HEALTHY_MS) and must
        // end one tick BEFORE UNHEALTHY_RUN_DURATION to count as unhealthy.
        let exits = vec![
            BASE + 1,
            BASE + 2,
            BASE + 2 + HEALTHY_MS,
            BASE + 2 + HEALTHY_MS + UNHEALTHY_MS - 1,
        ];
        let script = ClockScript::new(
            vec![
                Some(EXIT_RESTART),
                Some(EXIT_RESTART),
                Some(EXIT_RESTART),
                Some(EXIT_RESTART),
            ],
            exits,
        );

        *NOW_OVERRIDE.lock() = Some(BASE);
        let result = supervise(|| script.clone().step()).await;
        *NOW_OVERRIDE.lock() = None;

        assert!(result.is_ok(), "exhausted script ends clean: {result:?}");
        assert_eq!(
            *BACKOFF_DELAYS.lock(),
            vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                // Healthy-length run: no backoff recorded, counter cleared.
                // Post-reset crash: first step again.
                Duration::from_millis(100),
            ]
        );

        BACKOFF_DELAYS.lock().clear();
    }
}
