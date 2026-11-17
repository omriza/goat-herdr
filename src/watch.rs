//! `goat-herdr watch`: follows `hc events` for herdr-core and hands each
//! status transition to the caller. The stream ends when the core restarts
//! or its terminals are lost; the loop reconnects, and the new snapshot is
//! compared with the table so nothing in between is missed or repeated.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::hc::{self, Hc, Tracker, Transition};

/// A stream that lasted this long was healthy; the back-off starts over.
const HEALTHY: Duration = Duration::from_secs(60);

pub fn run(hc: &Hc, mut on_transition: impl FnMut(Transition)) -> Result<(), String> {
    let mut tracker = Tracker::default();
    let mut failures = 0u32;
    loop {
        let started = Instant::now();
        match follow(hc, &mut tracker, &mut on_transition) {
            Ok(()) => eprintln!("watch: hc events ended"),
            Err(err) => eprintln!("watch: {err}"),
        }
        if started.elapsed() > HEALTHY {
            failures = 0;
        }
        failures += 1;
        // Capped back-off; a core restart is back in seconds.
        std::thread::sleep(Duration::from_secs((u64::from(failures) * 2).min(30)));
    }
}

/// One `hc events` process, read to its end.
fn follow(
    hc: &Hc,
    tracker: &mut Tracker,
    on_transition: &mut impl FnMut(Transition),
) -> Result<(), String> {
    let mut child = Command::new(hc.bin())
        .args(["events", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| format!("run {} events: {err}", hc.bin().display()))?;
    let stdout = child.stdout.take().ok_or("hc events: no stdout")?;
    eprintln!("watch: following {} events", hc.bin().display());
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|err| format!("hc events: read: {err}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match hc::parse_line(&line) {
            Ok(parsed) => tracker
                .apply(parsed)
                .into_iter()
                .for_each(&mut *on_transition),
            // One odd line is not worth dropping the stream for.
            Err(err) => eprintln!("watch: {err}"),
        }
    }
    let status = child
        .wait()
        .map_err(|err| format!("hc events: wait: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("hc events exited with {status}"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::alert::Status;
    use std::path::PathBuf;

    /// A fake `hc` that prints the captured stream for `events`.
    fn fake_hc(name: &str) -> (Hc, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("goat-herdr-watch-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hc");
        let script = dir.join("hc");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = events ] || exit 2\ncat {fixtures}/snapshot.json {fixtures}/events.jsonl\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (Hc::new(script), dir)
    }

    #[test]
    fn follow_reads_the_stream_to_its_end() {
        let (hc, dir) = fake_hc("follow");
        let mut tracker = Tracker::default();
        let mut seen = Vec::new();
        follow(&hc, &mut tracker, &mut |t: Transition| seen.push(t.status)).unwrap();
        assert_eq!(seen.first(), Some(&Status::Blocked));
        assert_eq!(seen.last(), Some(&Status::Closed));
        assert_eq!(seen.iter().filter(|s| **s == Status::Blocked).count(), 6);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_binary_is_an_error() {
        let hc = Hc::new(PathBuf::from("/nonexistent/hc"));
        let err = follow(&hc, &mut Tracker::default(), &mut |_| {}).unwrap_err();
        assert!(err.starts_with("run /nonexistent/hc events:"), "{err}");
    }
}
