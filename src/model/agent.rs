//! Agents. v1 uses a fixed enum; the trait is reserved for
//! future extension. `(scope, agent)` maps uniquely to one skills root
//! (invariant I1).

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::error::{Result, SkmError};
use crate::model::config::{self, Scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Agent {
    /// The tool-agnostic `~/.agents/skills` convention (any agent that reads it).
    Agents,
    Claude,
    Codex,
}

impl Agent {
    /// All known agent variants — used for ghost/prune scanning and for
    /// deriving every user-facing "valid agents" hint (see [`valid_agents`]).
    pub const ALL: [Agent; 3] = [Agent::Agents, Agent::Claude, Agent::Codex];

    pub fn id(self) -> &'static str {
        match self {
            Agent::Agents => "agents",
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Agent::ALL.into_iter().find(|a| a.id() == s)
    }

    fn dir_name(self) -> &'static str {
        match self {
            Agent::Agents => ".agents",
            Agent::Claude => ".claude",
            Agent::Codex => ".codex",
        }
    }

    /// The skills root for this agent under the given scope.
    pub fn skills_root(self, scope: &Scope) -> Result<PathBuf> {
        let base = match scope {
            Scope::Global => config::home_dir()?,
            Scope::Project { base } => base.clone(),
        };
        Ok(base.join(self.dir_name()).join("skills"))
    }
}

/// Quoted, comma-joined list of valid agent ids. Single source so every hint stays in
/// sync with [`Agent::ALL`] when a variant is added.
pub fn valid_agents() -> String {
    Agent::ALL
        .iter()
        .map(|a| format!("'{}'", a.id()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pipe-joined ids for `--agent` usage hints.
pub fn agent_flag_choices() -> String {
    Agent::ALL
        .iter()
        .map(|a| a.id())
        .collect::<Vec<_>>()
        .join("|")
}

/// Error returned when parsing an unknown agent identifier.
#[derive(Debug, Clone)]
pub struct ParseAgentError(String);

impl fmt::Display for ParseAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown agent '{}'; valid agents are {}",
            self.0,
            valid_agents()
        )
    }
}

impl std::error::Error for ParseAgentError {}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Agent {
    type Err = ParseAgentError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Agent::from_id(s).ok_or_else(|| ParseAgentError(s.to_string()))
    }
}

/// Parse and validate a list of agent id strings.
pub fn parse_agents(ids: &[String]) -> Result<Vec<Agent>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for id in ids {
        let agent = Agent::from_id(id).ok_or_else(|| {
            SkmError::general(format!(
                "unknown agent '{id}'; valid agents are {}",
                valid_agents()
            ))
        })?;
        // Deduplicate while preserving insertion order.
        if seen.insert(agent) {
            out.push(agent);
        }
    }
    Ok(out)
}
