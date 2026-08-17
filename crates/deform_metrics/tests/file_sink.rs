//! The file sink is global state, so this is deliberately one test.

#![cfg(feature = "file")]

use std::fs;

use deform_metrics::RunInfo;

#[test]
fn writes_a_run_directory() {
    let dir = std::env::temp_dir().join(format!("deform-metrics-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    // SAFETY: single-threaded at this point, before any other thread reads the env.
    unsafe { std::env::set_var("DEFORM_METRICS_DIR", &dir) };

    deform_metrics::init(RunInfo {
        backend: "test",
        player: "PLAYER".into(),
        lobby_id: 7,
        tick_rate_micros: 16667,
        extra: vec![("scenario".into(), "smoke".into())],
    });

    deform_metrics::set_tick(41);
    deform_metrics::plot!("RTT", 12.5);
    deform_metrics::set_tick(42);
    {
        let _span = deform_metrics::span!("advance_local_simulation");
    }
    deform_metrics::event!("rollback", depth = 3u64, note = "mismatch");

    deform_metrics::flush();

    let run_dir = fs::read_dir(&dir)
        .expect("run parent directory")
        .next()
        .expect("one run directory")
        .expect("readable entry")
        .path();

    let run: serde_json::Value =
        serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run["backend"], "test");
    assert_eq!(run["lobby_id"], 7);
    assert_eq!(run["extra"]["scenario"], "smoke");

    let samples = fs::read_to_string(run_dir.join("samples.csv")).unwrap();
    let mut lines = samples.lines();
    assert_eq!(lines.next(), Some("t_us,tick,metric,value"));
    // `t_us` varies, so match on the columns that do not.
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split(',').skip(1).collect()).collect();
    assert_eq!(rows[0], ["41", "RTT", "12.5"]);
    // The span records elapsed micros under `<name>_us`, so only the key is fixed.
    assert_eq!(rows[1][0], "42");
    assert_eq!(rows[1][1], "advance_local_simulation_us");

    let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    let event: serde_json::Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(event["event"], "rollback");
    assert_eq!(event["tick"], 42);
    assert_eq!(event["depth"], 3);
    assert_eq!(event["note"], "mismatch");

    let _ = fs::remove_dir_all(&dir);
}
