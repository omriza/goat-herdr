//! What the bridge and the alert pipeline need from a Herdr: read a pane,
//! list agents, prompt, press keys. `herdr::PluginEnv` is the TUI; `hc::Hc`
//! is herdr-core.

use serde::Deserialize;

pub trait Backend: Send + Sync {
    /// The last `lines` of a pane's terminal, ANSI stripped.
    fn read_tail(&self, id: &str, lines: u32) -> Result<String, String>;
    /// Live agents.
    fn agents(&self) -> Result<Vec<AgentInfo>, String>;
    /// Text to an agent. Returns how it went in, for the reply line.
    fn prompt(&self, id: &str, text: &str) -> Result<&'static str, String>;
    /// Key presses, by name.
    fn send_keys(&self, id: &str, keys: &[&str]) -> Result<(), String>;
}

/// One live agent, the fields the bridge reads. `pane_id` is the backend's
/// id: `w3:p3` on the TUI, `t7` on herdr-core.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}
