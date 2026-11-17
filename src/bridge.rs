//! The two-way bridge: a detached daemon that turns chat input into Herdr
//! agent commands. One thread per bridge-capable sink; service specifics
//! live behind the `Bridge` trait.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::backend::{AgentInfo, Backend};
use crate::config::Config;
use crate::herdr::PluginEnv;
use crate::sink;
use crate::state::State;

/// One message or button press from a user, service-agnostic.
#[derive(Debug, Clone)]
pub struct Inbound {
    /// Routing thread the input arrived in, when the service has threads.
    pub thread_id: Option<i64>,
    pub from_user: i64,
    pub text: String,
    /// Set for button presses; the bridge acks these instead of replying.
    pub callback_id: Option<String>,
}

pub trait Bridge: Send {
    fn name(&self) -> &str;
    /// Blocks up to 25 s. Returns the inputs that arrived.
    fn poll(&self) -> Result<Vec<Inbound>, String>;
    /// Posts `text` where `inbound` came from. `pre` asks for a monospace
    /// block.
    fn reply(&self, inbound: &Inbound, text: &str, pre: bool) -> Result<(), String>;
    /// Acknowledges a button press with a short toast.
    fn ack(&self, inbound: &Inbound, text: &str) -> Result<(), String>;
    fn allowed_user(&self, user: i64) -> bool;
}

const HELP: &str = "In an agent's topic:\n\
  plain text      prompt the agent\n\
  /tail [n]       last n lines of the pane (default 40)\n\
  /keys k...      send keys, e.g. /keys y Enter or /keys esc\n\
  /status         this agent's state\n\
  /mute, /unmute  stop or resume this agent's alerts\n\
Anywhere:\n\
  /agents         list live agents\n\
  /help           this text";

/// Entry point for `goat-herdr bridge`. `detach` forks the daemon and returns
/// once it is running, which is what the startup hook needs.
pub fn run(env: &PluginEnv, config: &Config, detach: bool) -> Result<(), String> {
    let bridges = sink::build_bridges(config, &env.state_dir)?;
    if bridges.is_empty() {
        println!("bridge: no sink has bridge = true; nothing to run");
        return Ok(());
    }
    let state = State::new(&env.state_dir);
    if detach {
        return spawn_detached(&state);
    }
    let _guard = single_instance(&state)?;
    for thread in serve_all(bridges, Arc::new(env.clone()), &env.state_dir) {
        let _ = thread.join();
    }
    Ok(())
}

/// One polling thread per bridge, all driving `backend`. The caller holds
/// `single_instance` for as long as the threads run.
pub fn serve_all(
    bridges: Vec<Box<dyn Bridge>>,
    backend: Arc<dyn Backend>,
    state_dir: &Path,
) -> Vec<JoinHandle<()>> {
    bridges
        .into_iter()
        .map(|bridge| {
            let backend = Arc::clone(&backend);
            let state = State::new(state_dir);
            std::thread::spawn(move || serve(bridge, backend.as_ref(), &state))
        })
        .collect()
}

/// Starts `goat-herdr bridge` as its own process group with output in
/// `bridge.log`, after stopping any daemon from an earlier server.
fn spawn_detached(state: &State) -> Result<(), String> {
    stop_previous(state);
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let log_path = state.dir().join("bridge.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| format!("open {}: {err}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|err| format!("clone log handle: {err}"))?;
    let mut command = Command::new(exe);
    command
        .arg("bridge")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so the hook's exit does not take the daemon with it.
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("spawn bridge: {err}"))?;
    println!(
        "bridge: started pid {} (log: {})",
        child.id(),
        log_path.display()
    );
    Ok(())
}

/// Sends TERM to the pid in `bridge.pid` and waits briefly for its lock.
fn stop_previous(state: &State) {
    let Ok(pid) = std::fs::read_to_string(state.dir().join("bridge.pid")) else {
        return;
    };
    let pid = pid.trim();
    if pid.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-TERM", pid]).status();
    }
    for _ in 0..20 {
        if let Ok(file) = File::create(state.dir().join("bridge.lock")) {
            if file.try_lock().is_ok() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Holds `bridge.lock` for the life of the daemon and records the pid.
pub fn single_instance(state: &State) -> Result<File, String> {
    let path = state.dir().join("bridge.lock");
    let file = File::create(&path).map_err(|err| format!("create {}: {err}", path.display()))?;
    file.try_lock()
        .map_err(|_| "bridge: another instance holds bridge.lock".to_string())?;
    let mut pid = File::create(state.dir().join("bridge.pid"))
        .map_err(|err| format!("write bridge.pid: {err}"))?;
    let _ = writeln!(pid, "{}", std::process::id());
    Ok(file)
}

fn serve(bridge: Box<dyn Bridge>, backend: &dyn Backend, state: &State) {
    eprintln!("bridge {}: polling", bridge.name());
    let mut failures = 0u32;
    loop {
        match bridge.poll() {
            Ok(inbounds) => {
                failures = 0;
                for inbound in inbounds {
                    if !bridge.allowed_user(inbound.from_user) {
                        eprintln!(
                            "bridge {}: ignored user {}",
                            bridge.name(),
                            inbound.from_user
                        );
                        continue;
                    }
                    match handle(bridge.as_ref(), backend, state, &inbound) {
                        Ok(done) => eprintln!("bridge {}: {done}", bridge.name()),
                        Err(err) => {
                            eprintln!("bridge {}: error: {err}", bridge.name());
                            let _ = bridge.reply(&inbound, &format!("error: {err}"), false);
                        }
                    }
                }
            }
            Err(err) => {
                failures += 1;
                eprintln!("bridge {}: poll failed ({failures}): {err}", bridge.name());
                // Capped back-off; a DNS blip recovers in seconds.
                std::thread::sleep(Duration::from_secs((failures as u64 * 2).min(10)));
            }
        }
    }
}

/// Runs one input against Herdr. Returns a one-line record of what it did.
fn handle(
    bridge: &dyn Bridge,
    backend: &dyn Backend,
    state: &State,
    inbound: &Inbound,
) -> Result<String, String> {
    let text = inbound.text.trim();
    if inbound.callback_id.is_some() {
        return handle_button(bridge, backend, inbound, text);
    }
    let (command, rest) = split_command(text);
    match command.as_deref() {
        Some("help") | Some("start") => {
            bridge.reply(inbound, HELP, true)?;
            Ok("help".to_string())
        }
        Some("agents") => {
            let agents = backend.agents()?;
            bridge.reply(inbound, &agents_table(&agents), true)?;
            Ok("agents".to_string())
        }
        Some("tail") => {
            let pane = target_pane(backend, state, inbound)?;
            let lines: u32 = rest.trim().parse().unwrap_or(40);
            let tail = backend.read_tail(&pane, lines)?;
            bridge.reply(
                inbound,
                if tail.is_empty() { "(empty)" } else { &tail },
                true,
            )?;
            Ok(format!("tail {lines} of {pane}"))
        }
        Some("keys") => {
            let keys: Vec<&str> = rest.split_whitespace().collect();
            if keys.is_empty() {
                bridge.reply(inbound, "usage: /keys k... (e.g. /keys y Enter)", false)?;
                return Ok("keys usage".to_string());
            }
            let pane = target_pane(backend, state, inbound)?;
            backend.send_keys(&pane, &keys)?;
            bridge.reply(inbound, &format!("{pane} ← {}", keys.join(" ")), false)?;
            Ok(format!("keys {} -> {pane}", keys.join(" ")))
        }
        Some("status") => {
            let pane = target_pane(backend, state, inbound)?;
            let agents = backend.agents()?;
            let row = agents
                .iter()
                .filter(|a| a.pane_id == pane)
                .cloned()
                .collect::<Vec<_>>();
            bridge.reply(inbound, &agents_table(&row), true)?;
            Ok(format!("status of {pane}"))
        }
        Some(verb @ ("mute" | "unmute")) => {
            let pane = target_pane(backend, state, inbound)?;
            let muted = verb == "mute";
            let changed = state.set_muted(&pane, muted)?;
            let line = match (muted, changed) {
                (true, true) => format!("{pane} muted: no alerts until /unmute or it ends"),
                (false, true) => format!("{pane} unmuted"),
                (true, false) => format!("{pane} is already muted"),
                (false, false) => format!("{pane} is not muted"),
            };
            bridge.reply(inbound, &line, false)?;
            Ok(format!("{verb} {pane}"))
        }
        Some(other) => {
            bridge.reply(
                inbound,
                &format!("unknown command /{other}\n\n{HELP}"),
                true,
            )?;
            Ok(format!("unknown command /{other}"))
        }
        None => {
            if text.is_empty() {
                return Ok("empty".to_string());
            }
            let pane = target_pane(backend, state, inbound)?;
            let how = backend.prompt(&pane, text)?;
            bridge.reply(inbound, &format!("{how} {pane} ← {text}"), false)?;
            Ok(format!("{how} {pane}, {} chars", text.chars().count()))
        }
    }
}

/// Inline keyboard presses. Data is `k|<pane>|<key>...` or `t|<pane>`.
fn handle_button(
    bridge: &dyn Bridge,
    backend: &dyn Backend,
    inbound: &Inbound,
    data: &str,
) -> Result<String, String> {
    let mut parts = data.split('|');
    let (Some(kind), Some(pane)) = (parts.next(), parts.next()) else {
        bridge.ack(inbound, "bad button")?;
        return Ok(format!("bad button {data}"));
    };
    let alive = backend.agents()?.iter().any(|a| a.pane_id == pane);
    if !alive {
        bridge.ack(inbound, &format!("{pane} is gone"))?;
        return Ok(format!("button for gone pane {pane}"));
    }
    match kind {
        "k" => {
            let keys: Vec<&str> = parts.collect();
            backend.send_keys(pane, &keys)?;
            // The toast fades; the topic keeps a line.
            bridge.ack(inbound, &format!("sent {}", keys.join(" ")))?;
            let line = if keys.len() == 1 && keys[0].chars().all(|c| c.is_ascii_digit()) {
                format!("{pane} ← {}. Now send your text as a reply.", keys[0])
            } else {
                format!("{pane} ← {}", keys.join(" "))
            };
            bridge.reply(inbound, &line, false)?;
            Ok(format!("button keys {} -> {pane}", keys.join(" ")))
        }
        "t" => {
            let tail = backend.read_tail(pane, 40)?;
            bridge.ack(inbound, "tail")?;
            bridge.reply(
                inbound,
                if tail.is_empty() { "(empty)" } else { &tail },
                true,
            )?;
            Ok(format!("button tail of {pane}"))
        }
        _ => {
            bridge.ack(inbound, "bad button")?;
            Ok(format!("bad button {data}"))
        }
    }
}

/// `/tail 20` -> (Some("tail"), "20"); `/tail@bot` strips the mention.
fn split_command(text: &str) -> (Option<String>, &str) {
    let Some(rest) = text.strip_prefix('/') else {
        return (None, text);
    };
    let (word, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let word = word.split('@').next().unwrap_or(word);
    (Some(word.to_ascii_lowercase()), args)
}

/// The pane a topic's input goes to: a live pane registered under the
/// topic's key, preferring one that is blocked.
fn target_pane(backend: &dyn Backend, state: &State, inbound: &Inbound) -> Result<String, String> {
    let Some(thread_id) = inbound.thread_id else {
        return Err("send this inside an agent's topic".to_string());
    };
    let Some((key, panes)) = state.topic_panes_for_thread(thread_id)? else {
        return Err("this topic is not tied to an agent".to_string());
    };
    let agents = backend.agents()?;
    let mut live: Vec<&AgentInfo> = agents
        .iter()
        .filter(|a| panes.contains(&a.pane_id))
        .collect();
    if live.is_empty() {
        return Err(format!("no live agent for {}", key.replace('|', " · ")));
    }
    live.sort_by_key(|a| a.agent_status.as_deref() != Some("blocked"));
    Ok(live[0].pane_id.clone())
}

fn agents_table(agents: &[AgentInfo]) -> String {
    if agents.is_empty() {
        return "no agents".to_string();
    }
    agents
        .iter()
        .map(|a| {
            format!(
                "{:<8} {:<8} {:<8} {}",
                a.pane_id,
                a.agent.as_deref().unwrap_or("?"),
                a.agent_status.as_deref().unwrap_or("?"),
                a.cwd.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_and_strip_mentions() {
        assert_eq!(split_command("/tail 20"), (Some("tail".into()), "20"));
        assert_eq!(split_command("/tail@goat_bot"), (Some("tail".into()), ""));
        assert_eq!(
            split_command("/KEYS y Enter"),
            (Some("keys".into()), "y Enter")
        );
        assert_eq!(split_command("just text"), (None, "just text"));
    }

    struct FakeBridge(std::sync::Mutex<Vec<String>>);

    impl Bridge for FakeBridge {
        fn name(&self) -> &str {
            "fake"
        }
        fn poll(&self) -> Result<Vec<Inbound>, String> {
            Ok(Vec::new())
        }
        fn reply(&self, _: &Inbound, text: &str, _: bool) -> Result<(), String> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
        fn ack(&self, _: &Inbound, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn allowed_user(&self, _: i64) -> bool {
            true
        }
    }

    struct OneAgent;

    impl Backend for OneAgent {
        fn read_tail(&self, _: &str, _: u32) -> Result<String, String> {
            Ok(String::new())
        }
        fn agents(&self) -> Result<Vec<AgentInfo>, String> {
            Ok(vec![AgentInfo {
                pane_id: "t7".into(),
                agent: Some("claude-code".into()),
                agent_status: Some("done".into()),
                cwd: None,
            }])
        }
        fn prompt(&self, _: &str, _: &str) -> Result<&'static str, String> {
            Ok("prompted")
        }
        fn send_keys(&self, _: &str, _: &[&str]) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn mute_and_unmute_in_a_topic() {
        let dir = std::env::temp_dir().join(format!("goat-herdr-mute-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = State::new(&dir);
        state.topic_thread("k", "t7", || Ok(5)).unwrap();
        let bridge = FakeBridge(Default::default());
        let say = |text: &str| Inbound {
            thread_id: Some(5),
            from_user: 1,
            text: text.to_string(),
            callback_id: None,
        };
        handle(&bridge, &OneAgent, &state, &say("/mute")).unwrap();
        assert!(state.muted("t7"));
        handle(&bridge, &OneAgent, &state, &say("/mute")).unwrap();
        handle(&bridge, &OneAgent, &state, &say("/unmute")).unwrap();
        assert!(!state.muted("t7"));
        assert_eq!(
            *bridge.0.lock().unwrap(),
            [
                "t7 muted: no alerts until /unmute or it ends",
                "t7 is already muted",
                "t7 unmuted"
            ]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn agents_table_lists_rows() {
        let rows = vec![AgentInfo {
            pane_id: "w1:p1".into(),
            agent: Some("claude".into()),
            agent_status: Some("blocked".into()),
            cwd: Some("/x".into()),
        }];
        assert_eq!(agents_table(&rows), "w1:p1    claude   blocked  /x");
        assert_eq!(agents_table(&[]), "no agents");
    }
}
