//! Manifest builder — type-safe `skm.toml` construction.

use std::path::Path;

use super::write_file;

/// Builder for a complete `skm.toml`.
///
/// ```rust
/// Manifest::v1()
///     .default_agents(&["agents"])
///     .skill(Skill::git("s", &url).subdir("docx"))
///     .write_to(&env.manifest_path());
/// ```
pub struct Manifest {
    defaults: Option<Vec<String>>,
    entries: Vec<SkillEntry>,
}

/// Entry point for building a skill entry. Use the constructors on
/// the namespaced struct (`Skill::git(...)`, `Skill::zip(...)`, etc.).
pub struct SkillBuilder {
    entry: SkillEntry,
}

/// A single `[[skills]]` entry (internal, not pub).
#[derive(Clone)]
struct SkillEntry {
    name: String,
    source: SourceKind,
    subdir: Option<String>,
    ref_: Option<String>,
    sha256: Option<String>,
    agents: Option<Vec<String>>,
}

#[derive(Clone)]
enum SourceKind {
    Git { repo: String },
    Tar { url: String },
    Zip { url: String },
    Local { path: String },
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

impl Manifest {
    pub fn v1() -> Self {
        Manifest {
            defaults: None,
            entries: Vec::new(),
        }
    }

    /// Set `[defaults].agents`.
    pub fn default_agents(mut self, agents: &[&str]) -> Self {
        self.defaults = Some(agents.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Add a skill entry built by [`SkillBuilder`].
    pub fn skill(mut self, builder: SkillBuilder) -> Self {
        self.entries.push(builder.entry);
        self
    }

    /// Write the manifest to `path` (typically [`super::Env::manifest_path`]).
    pub fn write_to(&self, path: &Path) {
        write_file(path, &self.to_toml(), false);
    }

    /// Serialize to TOML string.
    pub fn to_toml(&self) -> String {
        let mut s = String::from("version = 1\n");
        if let Some(ref t) = self.defaults {
            s.push_str("\n[defaults]\n");
            let agents: Vec<String> = t.iter().map(|x| format!("\"{x}\"")).collect();
            s.push_str(&format!("agents = [{}]\n", agents.join(", ")));
        }
        for entry in &self.entries {
            s.push('\n');
            s.push_str(&entry.to_toml());
        }
        s
    }
}

// ---------------------------------------------------------------------------
// SkillBuilder – public constructors + setters
// ---------------------------------------------------------------------------

/// Namespace for skill constructors. Each returns a [`SkillBuilder`].
pub struct Skill;

impl Skill {
    pub fn git(name: &str, repo: &str) -> SkillBuilder {
        SkillBuilder {
            entry: SkillEntry::new(
                name,
                SourceKind::Git {
                    repo: repo.to_string(),
                },
            ),
        }
    }

    pub fn tar(name: &str, url: &str) -> SkillBuilder {
        SkillBuilder {
            entry: SkillEntry::new(
                name,
                SourceKind::Tar {
                    url: url.to_string(),
                },
            ),
        }
    }

    pub fn zip(name: &str, url: &str) -> SkillBuilder {
        SkillBuilder {
            entry: SkillEntry::new(
                name,
                SourceKind::Zip {
                    url: url.to_string(),
                },
            ),
        }
    }

    pub fn local(name: &str, path: &str) -> SkillBuilder {
        SkillBuilder {
            entry: SkillEntry::new(
                name,
                SourceKind::Local {
                    path: path.to_string(),
                },
            ),
        }
    }
}

impl SkillBuilder {
    /// Set the `subdir` field.
    pub fn subdir(mut self, p: &str) -> Self {
        self.entry.subdir = Some(p.to_string());
        self
    }

    /// Set the `ref` field (git only).
    pub fn ref_(mut self, r: &str) -> Self {
        self.entry.ref_ = Some(r.to_string());
        self
    }

    /// Set the `sha256` field (zip/tar only).
    pub fn sha256(mut self, s: &str) -> Self {
        self.entry.sha256 = Some(s.to_string());
        self
    }

    /// Set per-skill `agents` override.
    pub fn agents(mut self, t: &[&str]) -> Self {
        self.entry.agents = Some(t.iter().map(|s| s.to_string()).collect());
        self
    }
}

// ---------------------------------------------------------------------------
// SkillEntry – internal TOML serialization
// ---------------------------------------------------------------------------

impl SkillEntry {
    fn new(name: &str, source: SourceKind) -> Self {
        SkillEntry {
            name: name.to_string(),
            source,
            subdir: None,
            ref_: None,
            sha256: None,
            agents: None,
        }
    }

    fn to_toml(&self) -> String {
        let mut s = String::from("[[skills]]\n");
        s.push_str(&format!("name = \"{}\"\n", self.name));
        match &self.source {
            SourceKind::Git { repo } => s.push_str(&format!("git = \"{repo}\"\n")),
            SourceKind::Tar { url } => s.push_str(&format!("tar = \"{url}\"\n")),
            SourceKind::Zip { url } => s.push_str(&format!("zip = \"{url}\"\n")),
            SourceKind::Local { path } => s.push_str(&format!("local = \"{path}\"\n")),
        }
        if let Some(ref r) = self.ref_ {
            s.push_str(&format!("ref = \"{r}\"\n"));
        }
        if let Some(ref sd) = self.subdir {
            s.push_str(&format!("subdir = \"{sd}\"\n"));
        }
        if let Some(ref h) = self.sha256 {
            s.push_str(&format!("sha256 = \"{h}\"\n"));
        }
        if let Some(ref a) = self.agents {
            let agents: Vec<String> = a.iter().map(|x| format!("\"{x}\"")).collect();
            s.push_str(&format!("agents = [{}]\n", agents.join(", ")));
        }
        s
    }
}
