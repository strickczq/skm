//! `skm.toml` manifest: parse with `toml` (serde), rewrite with `toml_edit`
//! preserving formatting/comments.

use std::path::Path;

use serde::Deserialize;

use crate::error::{Result, SkmError, three_part};
use crate::model::lockfile;
use crate::source::{self, Source};
use crate::ui;

pub const MANIFEST_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub struct Manifest {
    pub default_agents: Option<Vec<String>>,
    pub skills: Vec<ManifestSkill>,
}

#[derive(Debug, Clone)]
pub struct ManifestSkill {
    pub name: String,
    pub source: Source,
    /// Explicit per-skill agents override (None → use `[defaults].agents`).
    pub agents_override: Option<Vec<String>>,
}

impl ManifestSkill {
    /// Effective agents: override if present, else defaults. Must be
    /// non-empty.
    pub fn effective_agents(&self, defaults: &Option<Vec<String>>) -> Result<Vec<String>> {
        let agents = match (&self.agents_override, defaults) {
            (Some(t), _) => t.clone(),
            (None, Some(d)) => d.clone(),
            (None, None) => Vec::new(),
        };
        if agents.is_empty() {
            return Err(SkmError::general(format!(
                "skill '{}' has no agents configured; set [defaults].agents or add an agents field",
                self.name
            )));
        }
        Ok(agents)
    }
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    version: Option<i64>,
    defaults: Option<RawDefaults>,
    #[serde(default)]
    skills: Vec<RawSkill>,
}

#[derive(Debug, Deserialize)]
struct RawDefaults {
    agents: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawSkill {
    name: String,
    git: Option<String>,
    tar: Option<String>,
    zip: Option<String>,
    local: Option<String>,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    subdir: Option<String>,
    sha256: Option<String>,
    agents: Option<Vec<String>>,
}

/// Validate a skill name: `[a-zA-Z0-9_-]+`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Parse manifest text into a validated [`Manifest`].
pub fn parse(text: &str) -> Result<Manifest> {
    let raw: RawManifest = toml::from_str(text).map_err(|e| {
        SkmError::general(three_part(
            "error: cannot parse skm.toml",
            &e.to_string(),
            "Fix the TOML syntax.",
        ))
    })?;

    // `version` is required and fail-closed: silently defaulting a missing field
    // to 1 would let a future field-less file be misread as v1.
    let version = match raw.version {
        None => {
            return Err(SkmError::general(
                "error: skm.toml is missing required 'version' field; add 'version = 1'",
            ));
        }
        Some(v) => v,
    };
    if version != MANIFEST_VERSION {
        return Err(SkmError::general(format!(
            "unsupported manifest version {version}; upgrade skm"
        )));
    }

    let default_agents = raw.defaults.and_then(|d| d.agents);

    let mut skills = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for rs in raw.skills {
        if !valid_name(&rs.name) {
            return Err(SkmError::general(format!(
                "invalid skill name '{}'; allowed characters: [a-zA-Z0-9_-]",
                rs.name
            )));
        }
        if seen.iter().any(|n| n == &rs.name) {
            return Err(SkmError::general(format!(
                "duplicate skill name '{}' in skm.toml",
                rs.name
            )));
        }
        // Case-only conflict warning.
        if let Some(other) = seen
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&rs.name) && *n != &rs.name)
        {
            ui::warn!(
                "skill names '{other}' and '{}' differ only in case; this is ambiguous on case-insensitive filesystems (macOS).",
                rs.name
            );
        }
        seen.push(rs.name.clone());

        let source = build_source(&rs)?;
        skills.push(ManifestSkill {
            name: rs.name,
            source,
            agents_override: rs.agents,
        });
    }

    Ok(Manifest {
        default_agents,
        skills,
    })
}

fn build_source(rs: &RawSkill) -> Result<Source> {
    let count = [
        rs.git.is_some(),
        rs.tar.is_some(),
        rs.zip.is_some(),
        rs.local.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if count != 1 {
        return Err(SkmError::general(format!(
            "skill '{}' must declare exactly one of git/tar/zip/local",
            rs.name
        )));
    }

    if rs.ref_.is_some() && rs.git.is_none() {
        return Err(SkmError::general(format!(
            "skill '{}': 'ref' is only valid with a git source",
            rs.name
        )));
    }
    if rs.sha256.is_some() && !(rs.zip.is_some() || rs.tar.is_some()) {
        return Err(SkmError::general(format!(
            "skill '{}': 'sha256' is only valid with a zip or tar source",
            rs.name
        )));
    }
    if rs.subdir.is_some() && rs.local.is_some() {
        return Err(SkmError::general(format!(
            "skill '{}': 'subdir' is not valid with a local source",
            rs.name
        )));
    }

    // Normalize git/zip/tar subdir here so '`./docx`' and '`docx`' unify
    // — git/zip/tar lock entries carry no `./` prefix; this also stops
    // identity_matches from triggering spurious re-resolves.
    let normalized_subdir = match &rs.subdir {
        Some(p) => Some(source::validate_source_subdir(p)?),
        None => None,
    };

    Ok(if let Some(repo) = &rs.git {
        Source::Git {
            repo: repo.clone(),
            ref_: rs.ref_.clone(),
            subdir: normalized_subdir,
        }
    } else if let Some(url) = &rs.tar {
        Source::Tar {
            url: url.clone(),
            subdir: normalized_subdir,
            sha256: rs.sha256.clone(),
        }
    } else if let Some(url) = &rs.zip {
        Source::Zip {
            url: url.clone(),
            subdir: normalized_subdir,
            sha256: rs.sha256.clone(),
        }
    } else {
        Source::Local {
            path: rs.local.clone().ok_or_else(|| {
                SkmError::general("internal: expected local source after validation")
            })?,
        }
    })
}

/// Load and parse a manifest file. Errors (with init hint) if missing.
pub fn load(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        SkmError::general(three_part(
            "error: no skm.toml found",
            &format!("expected a manifest at '{}'", path.display()),
            "Run `skm init` to create one.",
        ))
    })?;
    parse(&text)
}

/// The commented template written by `skm init`.
pub fn template() -> String {
    r#"version = 1                    # manifest schema version (required)

[defaults]
# Default agents for every skill. Uncomment and pick the agent(s) you
# use, or pass `--agent` on each `skm add`.
# Valid values: "agents", "codex", "claude" (any subset).
# ("agents" targets the tool-agnostic ~/.agents/skills directory.)
# agents = ["agents"]

# Declare skills below. Exactly one of git / tar / zip / local per entry.
#
# [[skills]]
# name = "docx"                   # install directory name (required, unique)
# git  = "https://github.com/anthrs/skills"
# ref  = "main"                   # optional: branch / tag / 40-hex commit
# subdir = "docx"                 # optional: subdirectory inside the source
# # agents = ["agents", "claude"] # optional: overrides [defaults].agents
#
# [[skills]]
# name   = "my-tool"
# zip    = "https://example.com/skills/my-tool.zip"
# sha256 = "..."                  # optional: verifies the downloaded bytes
# subdir = "my-tool"
#
# [[skills]]
# name  = "team-reviewer"
# local = "./vendor/reviewer"     # relative to this skm.toml
"#
    .to_string()
}

// ---------------------------------------------------------------------------
// toml_edit rewriting (add / remove)
// ---------------------------------------------------------------------------

use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// Load a `toml_edit` document for in-place editing, or start from the template.
pub fn load_doc(path: &Path) -> Result<DocumentMut> {
    let text = if path.is_file() {
        std::fs::read_to_string(path)
            .map_err(|e| SkmError::io(format!("cannot read '{}': {e}", path.display())))?
    } else {
        template()
    };
    text.parse::<DocumentMut>()
        .map_err(|e| SkmError::general(format!("cannot parse skm.toml for editing: {e}")))
}

pub fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    lockfile::atomic_write(path, &doc.to_string())
}

/// Atomically write raw manifest text (used by `init`).
pub fn atomic_write_str(path: &Path, contents: &str) -> Result<()> {
    lockfile::atomic_write(path, contents)
}

/// A description of a skill entry to append to the manifest.
pub struct NewEntry {
    pub name: String,
    pub source: Source,
    /// Explicit agents to write; `None` relies on `[defaults].agents`.
    pub agents: Option<Vec<String>>,
}

/// Append a `[[skills]]` entry to the document.
pub fn add_skill(doc: &mut DocumentMut, entry: &NewEntry) -> Result<()> {
    let skills = doc
        .entry("skills")
        .or_insert(Item::ArrayOfTables(Default::default()));
    let arr = skills
        .as_array_of_tables_mut()
        .ok_or_else(|| SkmError::general("skm.toml 'skills' is not an array of tables"))?;

    let mut t = Table::new();
    t.insert("name", toml_edit::value(entry.name.clone()));
    match &entry.source {
        Source::Git { repo, ref_, subdir } => {
            t.insert("git", toml_edit::value(repo.clone()));
            if let Some(r) = ref_ {
                t.insert("ref", toml_edit::value(r.clone()));
            }
            if let Some(sd) = subdir {
                t.insert("subdir", toml_edit::value(sd.clone()));
            }
        }
        Source::Tar {
            url,
            subdir,
            sha256,
        } => {
            t.insert("tar", toml_edit::value(url.clone()));
            if let Some(s) = sha256 {
                t.insert("sha256", toml_edit::value(s.clone()));
            }
            if let Some(p) = subdir {
                t.insert("subdir", toml_edit::value(p.clone()));
            }
        }
        Source::Zip {
            url,
            subdir,
            sha256,
        } => {
            t.insert("zip", toml_edit::value(url.clone()));
            if let Some(s) = sha256 {
                t.insert("sha256", toml_edit::value(s.clone()));
            }
            if let Some(p) = subdir {
                t.insert("subdir", toml_edit::value(p.clone()));
            }
        }
        Source::Local { path } => {
            t.insert("local", toml_edit::value(path.clone()));
        }
    }
    if let Some(agents) = &entry.agents {
        let mut a = Array::new();
        for agent in agents {
            a.push(Value::from(agent.clone()));
        }
        t.insert("agents", Item::Value(Value::Array(a)));
    }
    arr.push(t);
    Ok(())
}

/// Remove a `[[skills]]` entry by name. Returns true if removed.
pub fn remove_skill(doc: &mut DocumentMut, name: &str) -> bool {
    if let Some(skills) = doc
        .get_mut("skills")
        .and_then(|i| i.as_array_of_tables_mut())
    {
        let before = skills.len();
        skills.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        return skills.len() != before;
    }
    false
}
