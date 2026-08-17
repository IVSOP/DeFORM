//! The offline sink: buffer in memory, write to a run directory.
//!
//! # Layout
//!
//! One directory per run, under `DEFORM_METRICS_DIR` (default
//! `./deform-metrics`), named `<backend>-<unix_seconds>-<pid>`. The pid keeps
//! two clients on one machine from colliding.
//!
//! - `run.json` — what this run was: backend, player, lobby, tick rate, start time.
//! - `samples.csv` — `t_us,tick,metric,value`, long format. Pivot it and plot.
//! - `events.jsonl` — one JSON object per line, with whatever fields the event carried.
//!
//! # Why not write from the sim loop
//!
//! Async IO would not fix this. It would make every call site `.await`, and the
//! filesystem's variance would land inside the very loop whose microsecond
//! timing is being measured. Instead records go into a `Vec` (a push, ~ns) and
//! a plain OS thread — deliberately not a tokio task, so it cannot be scheduled
//! against the sim loop — swaps the buffer out every
//! [`FLUSH_INTERVAL_SECS`] and does the IO off to the side. The swap is O(1),
//! so the sim loop never waits on a write.
//!
//! At 60 Hz across a few dozen metrics this buffers on the order of a megabyte
//! per minute, which is why buffering is affordable in the first place.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::Value;

use crate::{RunInfo, current_tick, now_micros};

/// How often the background thread writes the buffer out. Bounds how much is
/// lost if the process is killed mid-run.
pub const FLUSH_INTERVAL_SECS: u64 = 5;

/// Buffer capacity reserved after each flush, so steady-state recording does
/// not reallocate.
const BUFFER_CAPACITY: usize = 8192;

struct Sample {
    t_us: u64,
    tick: Option<u64>,
    name: &'static str,
    value: f64,
}

struct Event {
    t_us: u64,
    tick: Option<u64>,
    name: &'static str,
    fields: Vec<(&'static str, Value)>,
}

struct Sink {
    dir: PathBuf,
    samples: Vec<Sample>,
    events: Vec<Event>,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
/// Checked before taking the lock, so recording costs one relaxed load when no
/// run is active.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FLUSHER: Once = Once::new();

/// Begin a run. Any previous run is flushed and closed first.
pub fn start(run: RunInfo) {
    flush();

    let parent = std::env::var("DEFORM_METRICS_DIR").unwrap_or_else(|_| "deform-metrics".into());
    let started_unix_us = now_micros();
    let dir = Path::new(&parent).join(format!(
        "{}-{}-{}",
        run.backend,
        started_unix_us / 1_000_000,
        std::process::id()
    ));

    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("deform_metrics: cannot create {}: {e}", dir.display());
        return;
    }

    if let Err(e) = write_run_json(&dir, &run, started_unix_us) {
        eprintln!("deform_metrics: cannot write run.json: {e}");
        return;
    }

    // Truncate and write the header once; every flush appends from here on.
    match File::create(dir.join("samples.csv")) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "t_us,tick,metric,value") {
                eprintln!("deform_metrics: cannot write samples.csv: {e}");
                return;
            }
        }
        Err(e) => {
            eprintln!("deform_metrics: cannot create samples.csv: {e}");
            return;
        }
    }
    if let Err(e) = File::create(dir.join("events.jsonl")) {
        eprintln!("deform_metrics: cannot create events.jsonl: {e}");
        return;
    }

    if let Ok(mut guard) = SINK.lock() {
        *guard = Some(Sink {
            dir,
            samples: Vec::with_capacity(BUFFER_CAPACITY),
            events: Vec::new(),
        });
    } else {
        return;
    }

    ACTIVE.store(true, Ordering::Release);

    FLUSHER.call_once(|| {
        std::thread::Builder::new()
            .name("deform-metrics-flush".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_secs(FLUSH_INTERVAL_SECS));
                    flush();
                }
            })
            .map(|_| ())
            .unwrap_or_else(|e| {
                eprintln!("deform_metrics: no flush thread ({e}); flush() is manual only")
            });
    });
}

fn write_run_json(dir: &Path, run: &RunInfo, started_unix_us: u64) -> std::io::Result<()> {
    let extra: serde_json::Map<String, Value> = run
        .extra
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v.clone())))
        .collect();

    let json = serde_json::json!({
        "backend": run.backend,
        "player": run.player,
        "lobby_id": run.lobby_id,
        "tick_rate_micros": run.tick_rate_micros,
        "started_unix_us": started_unix_us,
        "pid": std::process::id(),
        "extra": extra,
    });

    fs::write(dir.join("run.json"), serde_json::to_vec_pretty(&json)?)
}

#[inline]
pub(crate) fn push_sample(name: &'static str, value: f64) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let sample = Sample {
        t_us: now_micros(),
        tick: current_tick(),
        name,
        value,
    };
    if let Ok(mut guard) = SINK.lock()
        && let Some(sink) = guard.as_mut()
    {
        sink.samples.push(sample);
    }
}

pub(crate) fn push_event(name: &'static str, fields: &[(&'static str, Value)]) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let event = Event {
        t_us: now_micros(),
        tick: current_tick(),
        name,
        fields: fields.to_vec(),
    };
    if let Ok(mut guard) = SINK.lock()
        && let Some(sink) = guard.as_mut()
    {
        sink.events.push(event);
    }
}

/// Append everything buffered so far and clear the buffers.
pub fn flush() {
    // Swap the buffers out under the lock and do the IO outside it, so a slow
    // write never blocks a recording thread.
    let (dir, samples, events) = {
        let Ok(mut guard) = SINK.lock() else { return };
        let Some(sink) = guard.as_mut() else { return };
        if sink.samples.is_empty() && sink.events.is_empty() {
            return;
        }
        (
            sink.dir.clone(),
            std::mem::replace(&mut sink.samples, Vec::with_capacity(BUFFER_CAPACITY)),
            std::mem::take(&mut sink.events),
        )
    };

    if let Err(e) = append_samples(&dir, &samples) {
        eprintln!("deform_metrics: samples.csv write failed: {e}");
    }
    if let Err(e) = append_events(&dir, &events) {
        eprintln!("deform_metrics: events.jsonl write failed: {e}");
    }
}

fn append_samples(dir: &Path, samples: &[Sample]) -> std::io::Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .append(true)
        .open(dir.join("samples.csv"))?;
    let mut out = BufWriter::new(file);
    for s in samples {
        // Metric names are literals from the call sites, so they never need
        // CSV quoting. Keep them free of commas.
        match s.tick {
            Some(tick) => writeln!(out, "{},{},{},{}", s.t_us, tick, s.name, s.value)?,
            None => writeln!(out, "{},,{},{}", s.t_us, s.name, s.value)?,
        }
    }
    out.flush()
}

fn append_events(dir: &Path, events: &[Event]) -> std::io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let file = OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))?;
    let mut out = BufWriter::new(file);
    for e in events {
        let mut map = serde_json::Map::with_capacity(3 + e.fields.len());
        map.insert("t_us".into(), Value::from(e.t_us));
        map.insert("tick".into(), Value::from(e.tick));
        map.insert("event".into(), Value::from(e.name));
        for (key, value) in &e.fields {
            map.insert((*key).to_string(), value.clone());
        }
        writeln!(out, "{}", Value::Object(map))?;
    }
    out.flush()
}
