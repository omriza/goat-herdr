//! goat-herdr: a Herdr plugin that alerts you when an agent needs you.
//!
//! Herdr runs this binary as an event hook, a startup hook, or an action.
//! Each run is a short-lived process that reads its input from the
//! environment Herdr injects, so all I/O here is blocking. For herdr-core,
//! `watch` is one long-running process that follows `hc events` instead.

mod alert;
mod backend;
mod bridge;
mod config;
mod hc;
mod herdr;
mod http;
mod sink;
mod state;
mod watch;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use alert::{Alert, Status};
use backend::Backend;
use config::Config;
use herdr::PluginEnv;
use state::State;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("notify") => notify(),
        Some("test") => test(),
        Some("toggle") => toggle(),
        Some(verb @ ("mute" | "unmute")) => mute(verb == "mute", args.get(1).map(String::as_str)),
        Some("bridge") => bridge_cmd(args.iter().any(|a| a == "--detach")),
        Some("watch") => watch_cmd(&args[1..]),
        Some(other) => Err(format!("unknown subcommand: {other}")),
        None => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("goat-herdr: {err}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str =
    "usage: goat-herdr <notify|bridge [--detach]|watch [--hc PATH]|test|toggle|mute [ID]|unmute [ID]>";

/// Startup hook (`--detach`) or foreground daemon.
fn bridge_cmd(detach: bool) -> Result<(), String> {
    let env = PluginEnv::from_env()?;
    let config = Config::load(&env)?;
    bridge::run(&env, &config, detach)
}

/// Event hook entry point. Builds one alert from the Herdr event and hands it
/// to every configured sink.
fn notify() -> Result<(), String> {
    let env = PluginEnv::from_env()?;
    let config = Config::load(&env)?;
    let event_json = env
        .event_json
        .as_deref()
        .ok_or("HERDR_PLUGIN_EVENT_JSON is not set; this is an event hook")?;
    let event = herdr::parse_event(event_json)?;
    let context = match env.context_json.as_deref() {
        Some(json) => herdr::parse_context(json)?,
        None => herdr::Context::default(),
    };
    let Some(alert) = Alert::from_event(&event, &context, &config) else {
        return Ok(());
    };
    let debounce_key = alert.status.as_str();
    dispatch(&config, &env, &env.state_dir, alert, debounce_key)
}

/// Everything after an alert is built, for `notify` and `watch` alike:
/// closes clear the debounce entry; others respect pause and debounce, and a
/// blocked alert gets the pane's tail. `debounce_key` is what a repeat must
/// match to be dropped.
fn dispatch(
    config: &Config,
    backend: &dyn Backend,
    state_dir: &Path,
    mut alert: Alert,
    debounce_key: &str,
) -> Result<(), String> {
    let state = State::new(state_dir);
    if alert.status == Status::Closed {
        state.forget(&alert.pane_id)?;
    } else {
        if state.paused() || state.muted(&alert.pane_id) {
            return Ok(());
        }
        if !state.debounce(&alert.pane_id, debounce_key, config.alerts.debounce_secs)? {
            return Ok(());
        }
        if alert.status == Status::Blocked && config.alerts.tail_lines > 0 {
            // The alert goes out without a tail.
            let started = std::time::Instant::now();
            match backend.read_tail(&alert.pane_id, config.alerts.tail_lines) {
                Ok(tail) => alert.tail = Some(tail),
                Err(err) => eprintln!("goat-herdr: {err}"),
            }
            println!("tail: {}ms", started.elapsed().as_millis());
        }
    }
    deliver(config, state_dir, &alert)
}

/// herdr-core: follows `hc events`, alerts through the same pipeline as
/// `notify`, and runs the bridge in-process against `hc`. Runs until killed;
/// meant for a user service.
fn watch_cmd(args: &[String]) -> Result<(), String> {
    let hc_bin = match args {
        [] => PathBuf::from("hc"),
        [flag, path] if flag == "--hc" => PathBuf::from(path),
        _ => return Err("usage: goat-herdr watch [--hc PATH]".to_string()),
    };
    let env = PluginEnv::from_env()?;
    let config = Config::load(&env)?;
    let hc = Arc::new(hc::Hc::new(hc_bin));
    let bridges = sink::build_bridges(&config, &env.state_dir)?;
    let _guard = if bridges.is_empty() {
        None
    } else {
        Some(bridge::single_instance(&State::new(&env.state_dir))?)
    };
    let _threads = bridge::serve_all(bridges, hc.clone(), &env.state_dir);
    println!(
        "watch: config {}, state {}",
        env.config_dir.display(),
        env.state_dir.display()
    );
    let state = State::new(&env.state_dir);
    let prune = |tracker: &hc::Tracker| {
        let live: std::collections::HashSet<&str> = tracker.addresses().collect();
        match state.retain_muted(|id| live.contains(id)) {
            Ok(dropped) if !dropped.is_empty() => {
                println!("watch: unmuted ended sessions {}", dropped.join(", "));
            }
            Ok(_) => {}
            Err(err) => eprintln!("goat-herdr: {err}"),
        }
    };
    watch::run(&hc, prune, |transition| {
        let Some(alert) = Alert::from_hc(&transition, &config) else {
            return;
        };
        // Two different dialogs in a row are two decisions.
        let key = match &transition.detail {
            Some(detail) if alert.status == Status::Blocked => format!("blocked:{detail}"),
            _ => alert.status.as_str().to_string(),
        };
        if let Err(err) = dispatch(&config, hc.as_ref(), &env.state_dir, alert, &key) {
            eprintln!("goat-herdr: {err}");
        }
    })
}

/// Action entry point. Sends a fixed alert so the user can confirm the wiring.
fn test() -> Result<(), String> {
    let env = PluginEnv::from_env()?;
    let config = Config::load(&env)?;
    deliver(&config, &env.state_dir, &Alert::test(&config))
}

/// Action entry point. Pauses or resumes alerts.
fn toggle() -> Result<(), String> {
    let env = PluginEnv::from_env()?;
    let paused = State::new(&env.state_dir).toggle_paused()?;
    println!(
        "goat-herdr alerts {}",
        if paused { "paused" } else { "resumed" }
    );
    Ok(())
}

/// Mutes or unmutes one session's alerts. Without an id, the session this
/// runs in: `HC_TERMINAL_ID` under herdr-core, `HERDR_PANE_ID` in the TUI.
fn mute(muted: bool, id: Option<&str>) -> Result<(), String> {
    let id = id
        .map(str::to_string)
        .or_else(|| std::env::var("HC_TERMINAL_ID").ok())
        .or_else(|| std::env::var("HERDR_PANE_ID").ok())
        .filter(|id| !id.is_empty())
        .ok_or("which session? pass its id, e.g. goat-herdr mute t7")?;
    let env = PluginEnv::from_env()?;
    let changed = State::new(&env.state_dir).set_muted(&id, muted)?;
    let word = if muted { "muted" } else { "unmuted" };
    println!(
        "goat-herdr: {id} {}{word}",
        if changed { "" } else { "already " }
    );
    Ok(())
}

fn deliver(config: &Config, state_dir: &Path, alert: &Alert) -> Result<(), String> {
    let sinks = sink::build(config, state_dir)?;
    let mut failures = Vec::new();
    for sink in &sinks {
        let started = std::time::Instant::now();
        let outcome = sink.send(alert);
        let ms = started.elapsed().as_millis();
        match outcome {
            Ok(sink::Delivery::Sent) => {
                println!("{}: sent {} ({ms}ms)", sink.name(), alert.headline())
            }
            Ok(sink::Delivery::Skipped) => {}
            Err(err) => failures.push(format!("{}: {err} ({ms}ms)", sink.name())),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
