//! herdr-core through its CLI, `hc`: the `Backend` calls, the `hc events`
//! line format, and a table of terminals that turns those lines into status
//! transitions. Field names follow `hc schema` as of hc 0.1.21; see
//! tests/fixtures/hc for captures. docs/herdr-core.md maps all of it to the
//! TUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::alert::Status;
use crate::backend::{AgentInfo, Backend};

/// Pause between screen reads while waiting for the screen to settle.
const SETTLE_GAP: Duration = Duration::from_millis(300);
/// Extra reads at most: about two seconds.
const SETTLE_TRIES: u32 = 6;

pub struct Hc {
    bin: PathBuf,
}

impl Hc {
    pub fn new(bin: PathBuf) -> Self {
        Self { bin }
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// `hc read --lines N` is N lines of history above the whole screen,
    /// which is mostly blank on a tall headless terminal. Keep the last N
    /// lines that end in something. JSON, because the text form appends a
    /// note of its own when only the screen could be read.
    fn read_once(&self, id: &str, lines: u32) -> Result<String, String> {
        let out = self.run(&["read", id, "--lines", &lines.to_string(), "--json"])?;
        let read: ReadResult = serde_json::from_str(&out)
            .map_err(|err| format!("hc read: unexpected output: {err}"))?;
        Ok(last_lines(&read.text, lines as usize))
    }

    /// One terminal's facts.
    fn terminal(&self, id: &str) -> Result<Terminal, String> {
        let out = self.run(&["get", id, "--json"])?;
        serde_json::from_str(&out).map_err(|err| format!("hc get: unexpected output: {err}"))
    }

    /// Runs `hc` and returns stdout. A failing exit is an error with the
    /// `kind: message` hc puts on stderr.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(args)
            // Inside an hc terminal, hc would tag the text as a note from
            // that terminal; this text is from the person in the chat.
            .env_remove("HC_TERMINAL_ID")
            .output()
            .map_err(|err| format!("run {} {}: {err}", self.bin.display(), args[0]))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "hc {}: {}",
                args[0],
                hc_error(stderr.trim()).unwrap_or_else(|| stderr.trim().to_string())
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    }
}

impl Backend for Hc {
    /// The tail once the screen holds still. Claude Code reports `blocked`
    /// before it draws the dialog, so the first read can be the spinner.
    /// A busy screen never holds still; the cap bounds the wait.
    fn read_tail(&self, id: &str, lines: u32) -> Result<String, String> {
        let mut last = self.read_once(id, lines)?;
        for _ in 0..SETTLE_TRIES {
            std::thread::sleep(SETTLE_GAP);
            let now = self.read_once(id, lines)?;
            if now == last {
                break;
            }
            last = now;
        }
        Ok(last)
    }

    /// Terminals running an agent: hc names the program or it has reported
    /// a status. Plain shells have neither.
    fn agents(&self) -> Result<Vec<AgentInfo>, String> {
        let out = self.run(&["ls", "--json"])?;
        let list: ListCompact =
            serde_json::from_str(&out).map_err(|err| format!("hc ls: unexpected output: {err}"))?;
        Ok(list
            .terminals
            .into_iter()
            .filter(|t| t.agent.is_some() || t.status.is_some())
            .map(|t| AgentInfo {
                pane_id: t.address,
                agent: t.name.or(t.agent),
                agent_status: t.status,
                cwd: t.r#where.or(t.cwd),
            })
            .collect())
    }

    /// Idle or working: `hc send`. Blocked on a question: typed into the
    /// dialog with `--force`, after a free-text option was picked. Blocked on
    /// a permission prompt: refused, because the Enter after the text picks
    /// the highlighted option, which is "Yes".
    fn prompt(&self, id: &str, text: &str) -> Result<&'static str, String> {
        let terminal = self.terminal(id)?;
        match terminal.status.as_ref().filter(|s| s.state == "blocked") {
            None => {
                self.run(&["send", "--", id, text])?;
                Ok("prompted")
            }
            Some(status) if status.kind.as_deref() == Some("question") => {
                self.run(&["send", "--force", "--", id, text])?;
                Ok("typed into the open dialog of")
            }
            Some(status) => Err(format!(
                "{id} is waiting on a {} prompt{}; text would answer it. Use the buttons or /keys",
                status.kind.as_deref().unwrap_or("blocking"),
                status
                    .msg
                    .as_deref()
                    .filter(|m| !m.is_empty())
                    .map(|m| format!(" ({m})"))
                    .unwrap_or_default()
            )),
        }
    }

    fn send_keys(&self, id: &str, keys: &[&str]) -> Result<(), String> {
        let mut args = vec!["keys", id];
        args.extend_from_slice(keys);
        self.run(&args).map(|_| ())
    }
}

/// `{"error":{"kind":"x","message":"y"}}` -> `x: y`.
fn hc_error(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let error = value.get("error")?;
    let kind = error["kind"].as_str()?;
    let message = error["message"].as_str().unwrap_or("");
    Some(format!("{kind}: {message}"))
}

/// The last `n` lines, right-trimmed, after dropping trailing blank lines.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(n)..end].join("\n")
}

#[derive(Debug, Deserialize)]
struct ReadResult {
    text: String,
}

/// `hc ls --json`, the compact listing: `status` is a word here.
#[derive(Debug, Deserialize)]
struct ListCompact {
    terminals: Vec<ListRow>,
}

#[derive(Debug, Deserialize)]
struct ListRow {
    address: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    r#where: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// A `Terminal` from `hc get`, the events snapshot, or a `created` event;
/// the fields goat-herdr reads.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Terminal {
    pub address: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: String,
    /// `live` or `finished`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub status: Option<StatusRecord>,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    /// The program has sent a status report at least once: an agent.
    #[serde(default)]
    pub reported: bool,
    #[serde(default)]
    pub foreground: Option<Foreground>,
    /// The terminal title the program sets; Claude Code puts a summary of
    /// the conversation there.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub created_ms: Option<u64>,
}

/// One OSC 7501 record: the program's own words.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusRecord {
    /// idle, working, done, blocked or error.
    pub state: String,
    /// permission, question or auth, on blocked.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub msg: Option<String>,
    /// `claude-code`, `codex`...
    #[serde(default)]
    pub app: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Foreground {
    #[serde(default)]
    pub name: String,
}

impl Terminal {
    /// `machine` for `machine/t7`; None on this machine.
    pub fn machine(&self) -> Option<&str> {
        self.address.split_once('/').map(|(machine, _)| machine)
    }

    /// The name given at launch, else the program's own name for itself,
    /// else the foreground process.
    pub fn agent_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.status.as_ref().and_then(|s| s.app.clone()))
            .or_else(|| self.foreground.as_ref().map(|f| f.name.clone()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "agent".to_string())
    }

    /// A name for people: the space, the launch name and the title, each
    /// once; the address when they add nothing. Another machine's name goes
    /// last.
    pub fn session_name(&self) -> String {
        fn add(parts: &mut Vec<String>, part: &str) {
            let part = part.trim();
            if !part.is_empty() && !parts.iter().any(|p| p == part) {
                parts.push(part.to_string());
            }
        }
        let mut parts = Vec::new();
        add(&mut parts, &self.workspace());
        let before = parts.len();
        for part in [&self.name, &self.title].into_iter().flatten() {
            add(&mut parts, part);
        }
        if parts.len() == before {
            add(
                &mut parts,
                self.address.rsplit('/').next().unwrap_or_default(),
            );
        }
        if let Some(machine) = self.machine() {
            add(&mut parts, machine);
        }
        parts.join(" · ")
    }

    /// The Herdr app's space: the part of the `where` label before ` / `
    /// (`goat-herdr / main` -> `goat-herdr`). Without the label, the last
    /// folder of the launch directory.
    pub fn workspace(&self) -> String {
        if let Some(space) = self
            .labels
            .get("where")
            .map(|w| w.split(" / ").next().unwrap_or(w).trim())
            .filter(|s| !s.is_empty())
        {
            return space.to_string();
        }
        Path::new(&self.cwd)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// One line of `hc events --json`.
#[derive(Debug)]
pub enum Line {
    /// The first line, and the fresh state after `missed`.
    Snapshot { seq: u64, terminals: Vec<Terminal> },
    Event {
        seq: u64,
        terminal: String,
        change: Change,
    },
    /// Heartbeats and kinds goat-herdr does not read.
    Other,
}

#[derive(Debug)]
pub enum Change {
    Created(Box<Terminal>),
    Status(Option<StatusRecord>),
    Labels(HashMap<String, String>),
    Title(Option<String>),
    /// `finished` or `removed`.
    Gone,
    Other,
}

pub fn parse_line(line: &str) -> Result<Line, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|err| format!("hc events: bad line: {err}"))?;
    // The first line is the subscription result; `missed` carries the same
    // shape in params.
    let snapshot = if value.get("terminals").is_some() {
        Some(&value)
    } else if value["method"] == "missed" {
        value.get("params").filter(|p| p.get("terminals").is_some())
    } else {
        None
    };
    if let Some(snapshot) = snapshot {
        let terminals = serde_json::from_value(snapshot["terminals"].clone())
            .map_err(|err| format!("hc events: bad snapshot: {err}"))?;
        return Ok(Line::Snapshot {
            seq: snapshot["seq"].as_u64().unwrap_or(0),
            terminals,
        });
    }
    if value["method"] != "event" {
        return Ok(Line::Other);
    }
    let params = &value["params"];
    let (Some(seq), Some(terminal)) = (params["seq"].as_u64(), params["terminal"].as_str()) else {
        return Err("hc events: event without seq or terminal".to_string());
    };
    let data = &params["data"];
    let bad = |what: &str, err: serde_json::Error| format!("hc events: bad {what}: {err}");
    let change = match params["type"].as_str().unwrap_or("") {
        "created" => Change::Created(Box::new(
            serde_json::from_value(data.clone()).map_err(|e| bad("created", e))?,
        )),
        "status" => Change::Status(
            serde_json::from_value(data["status"].clone()).map_err(|e| bad("status", e))?,
        ),
        "labels" => Change::Labels(
            serde_json::from_value(data["labels"].clone()).map_err(|e| bad("labels", e))?,
        ),
        "title" => Change::Title(data["title"].as_str().map(str::to_string)),
        "finished" | "removed" => Change::Gone,
        _ => Change::Other,
    };
    Ok(Line::Event {
        seq,
        terminal: terminal.to_string(),
        change,
    })
}

/// A status change worth an alert, with what the terminal looked like then.
#[derive(Debug, Clone)]
pub struct Transition {
    pub terminal: Terminal,
    pub status: Status,
    /// The program's line for a blocked dialog, such as
    /// `approve Bash: cargo test`. Two dialogs in a row differ here.
    pub detail: Option<String>,
}

/// Live terminals as the event stream describes them. Status events carry
/// no name or labels, so the table keeps them from the snapshot and
/// `created` and `labels` events.
#[derive(Debug, Default)]
pub struct Tracker {
    terminals: HashMap<String, Terminal>,
    seq: u64,
    seeded: bool,
}

impl Tracker {
    /// Addresses of the live terminals in the table.
    pub fn addresses(&self) -> impl Iterator<Item = &str> {
        self.terminals.keys().map(String::as_str)
    }

    /// Applies one line. The first snapshot only fills the table: what was
    /// already blocked before goat-herdr started is not news. Later
    /// snapshots (after a reconnect or `missed`) report what changed.
    pub fn apply(&mut self, line: Line) -> Vec<Transition> {
        match line {
            Line::Snapshot { seq, terminals } => self.snapshot(seq, terminals),
            Line::Event {
                seq,
                terminal,
                change,
            } => {
                if seq <= self.seq {
                    return Vec::new();
                }
                self.seq = seq;
                self.event(terminal, change).into_iter().collect()
            }
            Line::Other => Vec::new(),
        }
    }

    fn snapshot(&mut self, seq: u64, terminals: Vec<Terminal>) -> Vec<Transition> {
        let fresh: HashMap<String, Terminal> = terminals
            .into_iter()
            .filter(|t| t.state == "live")
            .map(|t| (t.address.clone(), t))
            .collect();
        let old = std::mem::replace(&mut self.terminals, fresh);
        self.seq = seq;
        if !std::mem::replace(&mut self.seeded, true) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (address, before) in &old {
            match self.terminals.get(address) {
                None if before.reported => out.push(Transition {
                    terminal: before.clone(),
                    status: Status::Closed,
                    detail: None,
                }),
                None => {}
                Some(now) => out.extend(transition(before.status.as_ref(), now)),
            }
        }
        for (address, now) in &self.terminals {
            if !old.contains_key(address) {
                out.extend(transition(None, now));
            }
        }
        out.sort_by(|a, b| a.terminal.address.cmp(&b.terminal.address));
        out
    }

    fn event(&mut self, address: String, change: Change) -> Option<Transition> {
        match change {
            Change::Created(terminal) => {
                self.terminals.insert(address, *terminal);
                None
            }
            Change::Labels(labels) => {
                if let Some(t) = self.terminals.get_mut(&address) {
                    t.labels = labels;
                }
                None
            }
            Change::Title(title) => {
                if let Some(t) = self.terminals.get_mut(&address) {
                    t.title = title;
                }
                None
            }
            Change::Status(status) => {
                let entry = self.terminals.entry(address.clone()).or_insert(Terminal {
                    address,
                    state: "live".to_string(),
                    ..Terminal::default()
                });
                // A cleared status keeps the last report to compare with:
                // Claude Code clears it while it redraws a dialog.
                let status = status?;
                entry.reported = true;
                let before = entry.status.replace(status);
                transition(before.as_ref(), entry)
            }
            Change::Gone => {
                let gone = self.terminals.remove(&address)?;
                gone.reported.then_some(Transition {
                    terminal: gone,
                    status: Status::Closed,
                    detail: None,
                })
            }
            Change::Other => None,
        }
    }
}

/// A new state, or a different dialog while still blocked.
fn transition(before: Option<&StatusRecord>, now: &Terminal) -> Option<Transition> {
    let after = now.status.as_ref()?;
    let same = before.is_some_and(|b| {
        b.state == after.state && (after.state != "blocked" || b.msg == after.msg)
    });
    if same {
        return None;
    }
    Some(Transition {
        terminal: now.clone(),
        status: Status::parse(&after.state),
        detail: after.msg.clone().filter(|m| !m.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = include_str!("../tests/fixtures/hc/snapshot.json");
    const EVENTS: &str = include_str!("../tests/fixtures/hc/events.jsonl");
    const GET_BLOCKED: &str = include_str!("../tests/fixtures/hc/get_blocked.json");
    const READ_BLOCKED: &str = include_str!("../tests/fixtures/hc/read_blocked.json");
    const LS: &str = include_str!("../tests/fixtures/hc/ls.json");

    fn replay(tracker: &mut Tracker, lines: &str) -> Vec<Transition> {
        lines
            .lines()
            .flat_map(|l| tracker.apply(parse_line(l).unwrap()))
            .collect()
    }

    #[test]
    fn snapshot_keeps_live_terminals_and_alerts_nothing() {
        let mut tracker = Tracker::default();
        assert!(replay(&mut tracker, SNAPSHOT).is_empty());
        assert_eq!(tracker.terminals.len(), 1);
        let t8 = &tracker.terminals["t8"];
        assert_eq!(t8.workspace(), "goat-herdr");
        assert_eq!(t8.agent_name(), "claude-code");
        assert_eq!(t8.status.as_ref().unwrap().state, "working");
    }

    #[test]
    fn captured_session_yields_each_transition_once() {
        let mut tracker = Tracker::default();
        replay(&mut tracker, SNAPSHOT);
        let got: Vec<(String, Status, Option<String>)> = replay(&mut tracker, EVENTS)
            .into_iter()
            .map(|t| (t.terminal.address, t.status, t.detail))
            .collect();
        use Status::*;
        let b = |msg: &str| (Blocked, Some(msg.to_string()));
        let p = |status| (status, None);
        let want = vec![
            b("trust this folder"),
            p(Idle),
            p(Working),
            b("approve Bash: curl -sI https://example.com | head -1"),
            p(Working),
            p(Done),
            p(Working), // `working` with a new msg is not a transition
            p(Done),
            p(Working),
            p(Done),
            p(Working),
            b("approve Bash: mkdir -p made-by-agent && touch made-by-agent/x"),
            p(Working),
            p(Done),
            p(Working),
            b("approve Bash: touch made-by-agent/y"),
            p(Working),
            p(Done),
            p(Working),
            b("approve Bash: touch made-by-agent/z"),
            p(Working),
            p(Done),
            p(Working),
            b("answer: Which color do you prefer? (Red · Blue)"),
            p(Working),
            p(Done),
            p(Closed),
        ];
        let got_states: Vec<_> = got.iter().map(|g| (g.1, g.2.clone())).collect();
        assert_eq!(got_states, want);
        assert!(got.iter().all(|g| g.0 == "t9"));
        assert!(!tracker.terminals.contains_key("t9"));
    }

    #[test]
    fn named_terminal_keeps_name_and_labels_from_created_and_labels() {
        let mut tracker = Tracker::default();
        replay(&mut tracker, SNAPSHOT);
        let all = replay(&mut tracker, EVENTS);
        let blocked = all.iter().find(|t| t.status == Status::Blocked).unwrap();
        assert_eq!(blocked.terminal.agent_name(), "goat-test");
        assert_eq!(blocked.terminal.workspace(), "goat-test");
        let closed = all.last().unwrap();
        assert_eq!(closed.terminal.labels["where"], "goat-test / 1");
    }

    #[test]
    fn replayed_and_old_events_are_ignored() {
        let mut tracker = Tracker::default();
        replay(&mut tracker, SNAPSHOT);
        let first = replay(&mut tracker, EVENTS).len();
        assert!(first > 0);
        assert!(replay(&mut tracker, EVENTS).is_empty());
    }

    #[test]
    fn later_snapshot_reports_differences() {
        let mut tracker = Tracker::default();
        replay(&mut tracker, SNAPSHOT);
        // Reconnect: t8 went blocked, and a new agent t20 is done.
        let mut v: serde_json::Value = serde_json::from_str(SNAPSHOT).unwrap();
        v["seq"] = 900.into();
        v["terminals"][1]["status"] = serde_json::json!({"app": "claude-code", "kind": "question", "msg": "answer: ok?", "state": "blocked"});
        let mut t20 = v["terminals"][1].clone();
        t20["address"] = "t20".into();
        t20["status"] = serde_json::json!({"state": "done", "app": "codex"});
        v["terminals"].as_array_mut().unwrap().push(t20);
        let got = tracker.apply(parse_line(&v.to_string()).unwrap());
        let got: Vec<_> = got
            .iter()
            .map(|t| (t.terminal.address.as_str(), t.status))
            .collect();
        assert_eq!(got, [("t20", Status::Done), ("t8", Status::Blocked)]);
        // And agents that ended between streams are closed.
        let mut v: serde_json::Value = serde_json::from_str(SNAPSHOT).unwrap();
        v["seq"] = 901.into();
        v["terminals"][1]["state"] = "finished".into();
        let got = tracker.apply(parse_line(&v.to_string()).unwrap());
        let got: Vec<_> = got
            .iter()
            .map(|t| (t.terminal.address.as_str(), t.status))
            .collect();
        assert_eq!(got, [("t20", Status::Closed), ("t8", Status::Closed)]);
    }

    #[test]
    fn status_for_unknown_terminal_still_alerts() {
        let mut tracker = Tracker::default();
        let line = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"records":{},"reported":true,"status":{"app":"claude-code","kind":"permission","msg":"approve Bash: ls","state":"blocked"}},"seq":5,"terminal":"mac/t3","type":"status"}}"#;
        let got = tracker.apply(parse_line(line).unwrap());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].terminal.machine(), Some("mac"));
        assert_eq!(got[0].terminal.agent_name(), "claude-code");
    }

    #[test]
    fn heartbeat_and_unread_kinds_are_other() {
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","method":"heartbeat","params":{"seq":168}}"#).unwrap(),
            Line::Other
        ));
        assert!(matches!(
            parse_line(r#"{"jsonrpc":"2.0","method":"event","params":{"data":{},"seq":172,"terminal":"t9","type":"turn"}}"#).unwrap(),
            Line::Event { change: Change::Other, .. }
        ));
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn title_events_update_the_table() {
        let mut tracker = Tracker::default();
        let snapshot =
            r#"{"seq":1,"terminals":[{"address":"t9","state":"live","title":"Claude Code"}]}"#;
        tracker.apply(parse_line(snapshot).unwrap());
        let line = r#"{"jsonrpc":"2.0","method":"event","params":{"data":{"title":"Fix the login bug"},"seq":2,"terminal":"t9","type":"title"}}"#;
        assert!(tracker.apply(parse_line(line).unwrap()).is_empty());
        assert_eq!(
            tracker.terminals["t9"].title.as_deref(),
            Some("Fix the login bug")
        );
    }

    #[test]
    fn session_name_reads_like_the_session() {
        let mut t = Terminal {
            address: "t8".into(),
            cwd: "/home/a/src/goat-herdr".into(),
            ..Terminal::default()
        };
        assert_eq!(t.session_name(), "goat-herdr · t8");
        t.labels.insert("where".into(), "tavern / main".into());
        t.title = Some("Herdr agent tavern simulation".into());
        assert_eq!(t.session_name(), "tavern · Herdr agent tavern simulation");
        t.name = Some("sim".into());
        assert_eq!(
            t.session_name(),
            "tavern · sim · Herdr agent tavern simulation"
        );
        t.name = Some("tavern".into());
        t.title = Some(" ".into());
        assert_eq!(t.session_name(), "tavern · t8");
        t.address = "mac/t3".into();
        assert_eq!(t.session_name(), "tavern · t3 · mac");
    }

    #[test]
    fn missed_carries_a_snapshot() {
        let line = r#"{"jsonrpc":"2.0","method":"missed","params":{"seq":40,"terminals":[]}}"#;
        assert!(matches!(
            parse_line(line).unwrap(),
            Line::Snapshot { seq: 40, .. }
        ));
    }

    #[test]
    fn last_lines_drops_trailing_blank_screen() {
        let screen = "a\nb   \n\nc\n\n   \n\n";
        assert_eq!(last_lines(screen, 2), "\nc");
        assert_eq!(last_lines(screen, 10), "a\nb\n\nc");
        assert_eq!(last_lines("\n\n", 5), "");
    }

    #[test]
    fn workspace_falls_back_to_cwd() {
        let t = Terminal {
            cwd: "/home/x/src/app".into(),
            ..Terminal::default()
        };
        assert_eq!(t.workspace(), "app");
        assert_eq!(t.agent_name(), "agent");
        assert_eq!(t.machine(), None);
    }

    #[test]
    fn hc_error_reads_kind_and_message() {
        assert_eq!(
            hc_error(
                r#"{"error":{"hint":"h","kind":"blocked","message":"t9 is blocked (permission)"}}"#
            ),
            Some("blocked: t9 is blocked (permission)".to_string())
        );
        assert_eq!(hc_error("plain"), None);
    }

    /// A fake `hc` in a temp dir: logs its arguments to `calls`, answers
    /// `get`, `read` and `ls` from files beside it, and fails `send` with
    /// hc's blocked error when `send-fails` exists.
    #[cfg(unix)]
    fn fake_hc(name: &str, get: &str) -> (Hc, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("goat-herdr-hc-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("get.json"), get).unwrap();
        std::fs::write(dir.join("read.json"), READ_BLOCKED).unwrap();
        std::fs::write(dir.join("ls.json"), LS).unwrap();
        let d = dir.display();
        let script = format!(
            r#"#!/bin/sh
printf '%s|' "$@" >> {d}/calls; echo >> {d}/calls
case "$1" in
  get) cat {d}/get.json ;;
  read) cat {d}/read.json ;;
  ls) cat {d}/ls.json ;;
  send) if [ -e {d}/send-fails ]; then
          echo '{{"error":{{"hint":"h","kind":"blocked","message":"t10 is blocked (permission)"}}}}' >&2; exit 9
        fi; echo '{{"turn":1}}' ;;
  keys) echo '{{"turn":1}}' ;;
  *) exit 2 ;;
esac
"#
        );
        std::fs::write(dir.join("hc"), script).unwrap();
        std::fs::set_permissions(dir.join("hc"), std::fs::Permissions::from_mode(0o755)).unwrap();
        (Hc::new(dir.join("hc")), dir)
    }

    #[cfg(unix)]
    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn with_status(status: &str) -> String {
        let mut v: serde_json::Value = serde_json::from_str(GET_BLOCKED).unwrap();
        v["status"] = serde_json::from_str(status).unwrap();
        v.to_string()
    }

    #[test]
    #[cfg(unix)]
    fn prompt_refuses_text_on_a_permission_prompt() {
        let (hc, dir) = fake_hc("perm", GET_BLOCKED);
        let err = hc.prompt("t10", "yes").unwrap_err();
        assert_eq!(
            err,
            "t10 is waiting on a permission prompt (approve Bash: touch fixture-prompt); text would answer it. Use the buttons or /keys"
        );
        assert_eq!(calls(&dir), ["get|t10|--json|"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn prompt_types_into_a_question_with_force() {
        let get = with_status(
            r#"{"app":"claude-code","kind":"question","msg":"answer: x?","state":"blocked"}"#,
        );
        let (hc, dir) = fake_hc("question", &get);
        assert_eq!(
            hc.prompt("t10", "green").unwrap(),
            "typed into the open dialog of"
        );
        assert_eq!(calls(&dir)[1], "send|--force|--|t10|green|");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn prompt_sends_when_not_blocked() {
        let (hc, dir) = fake_hc(
            "idle",
            &with_status(r#"{"app":"claude-code","state":"idle"}"#),
        );
        assert_eq!(hc.prompt("t10", "-v go").unwrap(), "prompted");
        assert_eq!(calls(&dir)[1], "send|--|t10|-v go|");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn send_error_carries_hc_kind_and_message() {
        let (hc, dir) = fake_hc("race", &with_status("null"));
        std::fs::write(dir.join("send-fails"), "").unwrap();
        assert_eq!(
            hc.prompt("t10", "hi").unwrap_err(),
            "hc send: blocked: t10 is blocked (permission)"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn read_tail_keeps_the_dialog_without_hc_notes() {
        let (hc, dir) = fake_hc("read", GET_BLOCKED);
        let tail = hc.read_tail("t10", 8).unwrap();
        assert_eq!(tail.lines().count(), 8);
        assert!(tail.contains("1. Yes"), "{tail}");
        assert!(tail.ends_with("Esc to cancel · Tab to amend"), "{tail}");
        assert!(!tail.contains("[hc]"));
        // A still screen: one read, one read to confirm.
        assert_eq!(
            calls(&dir),
            ["read|t10|--lines|8|--json|", "read|t10|--lines|8|--json|"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn agents_come_from_ls_with_name_and_where() {
        let (hc, dir) = fake_hc("ls", GET_BLOCKED);
        let agents = hc.agents().unwrap();
        let rows: Vec<_> = agents
            .iter()
            .map(|a| {
                (
                    a.pane_id.as_str(),
                    a.agent.as_deref().unwrap(),
                    a.agent_status.as_deref().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("t7", "pr-review", "done"),
                ("t8", "claude-code", "working"),
                ("t10", "goat-test", "blocked")
            ]
        );
        assert_eq!(agents[1].cwd.as_deref(), Some("goat-herdr / herdr-core"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn keys_pass_through() {
        let (hc, dir) = fake_hc("keys", GET_BLOCKED);
        hc.send_keys("t10", &["1", "Enter"]).unwrap();
        assert_eq!(calls(&dir), ["keys|t10|1|Enter|"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn get_fixture_parses() {
        let t: Terminal = serde_json::from_str(GET_BLOCKED).unwrap();
        assert_eq!(t.address, "t10");
        assert_eq!(t.agent_name(), "goat-test");
        let status = t.status.unwrap();
        assert_eq!(status.kind.as_deref(), Some("permission"));
    }
}
