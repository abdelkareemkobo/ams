use std::{collections::BTreeMap, fmt, str::FromStr};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Omp,
    Pi,
    Codex,
    Opencode,
    Claude,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Omp => "omp",
            Self::Pi => "pi",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Claude => "claude",
        })
    }
}

impl FromStr for Format {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "omp" => Ok(Self::Omp),
            "pi" => Ok(Self::Pi),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            "claude" => Ok(Self::Claude),
            _ => anyhow::bail!(
                "unknown agent '{value}'; expected omp, pi, codex, opencode, or claude"
            ),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub format: Format,
    pub native_id: String,
    pub title: String,
    pub cwd: String,
    pub started_at: String,
    pub events: Vec<Event>,
    pub losses: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Text {
        role: String,
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        id: String,
        name: String,
        text: String,
        is_error: bool,
    },
    Image {
        role: String,
        mime_type: String,
        data: String,
    },
    Summary {
        text: String,
    },
}
