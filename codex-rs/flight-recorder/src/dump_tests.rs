use std::fs;

use pretty_assertions::assert_eq;

use super::DUMP_RETENTION;
use super::Process;
use crate::Budgets;
use crate::FlightRecorder;

#[test]
fn on_demand_dump_prunes_only_its_own_process_and_cause() {
    let dir = tempfile::tempdir().expect("dump dir");
    let mut stale = Vec::new();
    for unix_ms in 1..=5 {
        for name in [
            format!("tui.signal.1.{unix_ms}.log"),
            format!("tui.panic.1.{unix_ms}.log"),
            format!("app-server.signal.1.{unix_ms}.log"),
        ] {
            fs::write(dir.path().join(&name), "").expect("seed dump");
            stale.push(name);
        }
    }

    FlightRecorder::new(Budgets::CLIENT)
        .dump(dir.path(), Process::Tui)
        .expect("dump");

    let mut names = fs::read_dir(dir.path())
        .expect("list dumps")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("utf-8")
        })
        .collect::<Vec<_>>();
    names.sort();
    let count = |prefix: &str| names.iter().filter(|name| name.starts_with(prefix)).count();
    assert_eq!(
        (
            count("tui.signal."),
            count("tui.panic."),
            count("app-server.signal.")
        ),
        (DUMP_RETENTION, 5, 5)
    );
    // The fresh dump survives; the oldest seeded ones go.
    assert!(!names.contains(&"tui.signal.1.1.log".to_owned()));
    assert!(!names.contains(&"tui.signal.1.2.log".to_owned()));
}
