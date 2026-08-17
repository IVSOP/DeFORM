//! Measurement facade for DeFORM: one call site, two sinks.
//!
//! - **tracy** — live plots/spans/messages while the game runs.
//! - **file** — every record is buffered in memory and written to a run
//!   directory for offline analysis (see [`file`]).
//!
//! Which sinks exist is decided by *this* crate's features. That matters:
//! `#[cfg]` inside a `macro_rules!` expansion is evaluated against the
//! **caller's** features, so all sink gating happens at macro *definition*
//! time here, and the expansions call plain functions that are no-ops when a
//! sink is compiled out. Call sites therefore never carry sink `#[cfg]`s of
//! their own — only the one `#[cfg(feature = "metrics")]` that decides whether
//! this crate is a dependency at all.
//!
//! ```ignore
//! deform_metrics::set_tick(self.local_tick);
//! deform_metrics::plot!("RTT", rtt.as_secs_f64() * 1000.0);
//! let _span = deform_metrics::span!("advance_local_simulation");
//! deform_metrics::event!("rollback", depth = 4u64, to_tick = 120u64);
//! ```
//!
//! # Timestamps
//!
//! Records are stamped with microseconds since the Unix epoch, not with a
//! monotonic clock. Two clients running on the same machine therefore share a
//! time base, and their runs can be compared directly. Every record also
//! carries the simulation tick it belongs to (see [`set_tick`]), so two runs of
//! the same match can be joined on `(lobby_id, tick)` even when the clocks are
//! not comparable.

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "file")]
pub mod file;

#[doc(hidden)]
pub use serde_json;
#[cfg(feature = "tracy")]
#[doc(hidden)]
pub use tracy_client;

// ---------------------------------------------------------------------------
// run identity
// ---------------------------------------------------------------------------

/// Everything needed to tell one run from another when the graphs get drawn
/// weeks later. Written verbatim into the run directory's `run.json`.
#[derive(Debug, Clone)]
pub struct RunInfo {
    /// Which backend produced this run: `"quic"`, `"foc"`, `"offline"`.
    pub backend: &'static str,
    /// The local player, so two clients in one match can be told apart.
    pub player: String,
    pub lobby_id: u64,
    pub tick_rate_micros: u64,
    /// Free-form, for whatever else distinguishes this run: the RTT probe in
    /// use, the ER region, the network, a scenario name.
    pub extra: Vec<(String, String)>,
}

/// Start recording into a fresh run directory.
///
/// The parent directory comes from `DEFORM_METRICS_DIR` and defaults to
/// `./deform-metrics`. Calling this again ends the previous run.
///
/// No-op unless the `file` feature is on; Tracy needs no setup.
pub fn init(run: RunInfo) {
    #[cfg(feature = "file")]
    file::start(run);
    #[cfg(not(feature = "file"))]
    let _ = run;
}

/// Write out everything buffered so far. Called automatically every
/// [`file::FLUSH_INTERVAL_SECS`]; call it explicitly when a run ends.
pub fn flush() {
    #[cfg(feature = "file")]
    file::flush();
}

// ---------------------------------------------------------------------------
// tick attribution
// ---------------------------------------------------------------------------

const NO_TICK: u64 = u64::MAX;
static CURRENT_TICK: AtomicU64 = AtomicU64::new(NO_TICK);

/// Attribute every subsequent record to this simulation tick.
///
/// Backends set this once per tick. It is what makes records from two
/// different clients joinable without trusting either machine's clock.
#[inline]
pub fn set_tick(tick: u64) {
    CURRENT_TICK.store(tick, Ordering::Relaxed);
}

/// The tick records are currently attributed to, if [`set_tick`] was ever called.
#[inline]
pub fn current_tick() -> Option<u64> {
    match CURRENT_TICK.load(Ordering::Relaxed) {
        NO_TICK => None,
        tick => Some(tick),
    }
}

/// Microseconds since the Unix epoch.
#[inline]
pub fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// macros
// ---------------------------------------------------------------------------

/// Record a numeric sample. The name must be a literal.
///
/// Goes to Tracy as a plot and to the run directory as a row in `samples.csv`.
/// The value expression still runs whenever this crate is a dependency, so a
/// costly one belongs behind the caller's own `metrics` feature gate — which is
/// where the call site already sits.
#[macro_export]
macro_rules! plot {
    ($name:literal, $value:expr) => {{
        let __value: f64 = $value;
        $crate::__tracy_plot!($name, __value);
        $crate::record_sample($name, __value);
    }};
}

/// Time a scope. The returned guard records the elapsed time when dropped.
///
/// Goes to Tracy as a span and to the run directory as a sample named
/// `<name>_us`.
#[macro_export]
macro_rules! span {
    ($name:literal) => {
        $crate::Span::new(concat!($name, "_us"), $crate::__tracy_span!($name))
    };
}

/// Record a discrete occurrence, with optional numeric or string fields.
///
/// Goes to Tracy as a message and to the run directory as a line in
/// `events.jsonl`. Unlike [`plot!`], events keep their structure, so a rollback
/// can carry its depth and its magnitude in one record.
///
/// ```ignore
/// deform_metrics::event!("rollback");
/// deform_metrics::event!("rollback", depth = 4u64, magnitude = 12.5);
/// ```
#[macro_export]
macro_rules! event {
    ($name:literal $(, $field:ident = $value:expr)* $(,)?) => {{
        $crate::record_event(
            $name,
            &[$((
                stringify!($field),
                $crate::serde_json::Value::from($value),
            )),*],
        );
    }};
}

// Sink gating lives here, not in the expansion, so the caller's own features
// are never consulted.

#[cfg(feature = "tracy")]
#[doc(hidden)]
#[macro_export]
macro_rules! __tracy_plot {
    ($name:literal, $value:expr) => {
        $crate::tracy_client::plot!($name, $value)
    };
}

#[cfg(not(feature = "tracy"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __tracy_plot {
    ($name:literal, $value:expr) => {
        let _ = $value;
    };
}

#[cfg(feature = "tracy")]
#[doc(hidden)]
#[macro_export]
macro_rules! __tracy_span {
    ($name:literal) => {
        $crate::tracy_client::span!($name)
    };
}

#[cfg(not(feature = "tracy"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __tracy_span {
    ($name:literal) => {
        ()
    };
}

// ---------------------------------------------------------------------------
// spans
// ---------------------------------------------------------------------------

/// The Tracy span a [`Span`] wraps, or `()` when Tracy is compiled out.
#[cfg(feature = "tracy")]
pub type TracySpan = tracy_client::Span;
/// The Tracy span a [`Span`] wraps, or `()` when Tracy is compiled out.
#[cfg(not(feature = "tracy"))]
pub type TracySpan = ();

/// Scope timer produced by [`span!`]. Records on drop; keep it alive with a
/// binding (`let _span = ...`, not `let _ = ...`).
#[must_use = "a span records nothing unless it is bound for the scope it times"]
pub struct Span {
    _tracy: TracySpan,
    name: &'static str,
    start: std::time::Instant,
}

impl Span {
    #[doc(hidden)]
    pub fn new(name: &'static str, tracy: TracySpan) -> Self {
        Self {
            _tracy: tracy,
            name,
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        record_sample(self.name, self.start.elapsed().as_micros() as f64);
    }
}

// ---------------------------------------------------------------------------
// sink entry points
// ---------------------------------------------------------------------------

/// Record a numeric sample. Prefer [`plot!`], which also feeds Tracy.
#[cfg(feature = "file")]
#[inline]
pub fn record_sample(name: &'static str, value: f64) {
    file::push_sample(name, value);
}

/// Record a numeric sample. Prefer [`plot!`], which also feeds Tracy.
#[cfg(not(feature = "file"))]
#[inline(always)]
pub fn record_sample(_name: &'static str, _value: f64) {}

/// Record a structured occurrence. Prefer [`event!`].
#[doc(hidden)]
pub fn record_event(name: &'static str, fields: &[(&'static str, serde_json::Value)]) {
    #[cfg(feature = "tracy")]
    {
        // Events are rare (rollbacks, gaps, failed commits), so formatting here
        // is not on any hot path. Tracy takes a copy of the string either way.
        let mut message = String::from(name);
        for (key, value) in fields {
            message.push(' ');
            message.push_str(key);
            message.push('=');
            message.push_str(&value.to_string());
        }
        tracy_client::Client::running()
            .expect("tracy client is always running when the `enable` feature is set")
            .message(&message, 0);
    }

    #[cfg(feature = "file")]
    file::push_event(name, fields);

    #[cfg(not(any(feature = "tracy", feature = "file")))]
    let _ = (name, fields);
}
