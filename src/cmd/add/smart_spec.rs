//! Smart spec parsing for `skm add`: turn the user's command-line source
//! specification into a typed [`Source`] plus an inferred skill name.
//!
//! This is the pure, side-effect-light half of `add` (the only I/O is a disk
//! probe that recognizes an existing *bare* directory path as a local source;
//! `owner/repo` shorthand is classified structurally and never depends on the
//! cwd, so the same spec resolves identically on every machine). It lives in
//! `cmd/` — not `model/` or `source/` — because it interprets CLI input
//! (`AddArgs`, name inference, flag-compatibility checks), and because it
//! depends on `AddArgs`; pushing it lower would invert the layering.

use std::path::Path;

use super::AddArgs;
use crate::error::{Result, SkmError, three_part};
use crate::model::manifest;
use crate::source::{Source, validate_source_subdir};

/// A parsed `add` spec: the resolved [`Source`] plus the locator kind it came
/// from (kept for name inference).
pub struct SmartSpec {
    pub source: Source,
    /// For name inference: the locator the name derives from.
    kind: SmartSpecKind,
}

#[derive(PartialEq)]
enum SmartSpecKind {
    Git,
    Tar,
    Zip,
    Local,
}

impl SmartSpecKind {
    /// The explicit `git|tar|zip|local` subcommand keyword, if it is one.
    fn from_keyword(s: &str) -> Option<SmartSpecKind> {
        match s {
            "git" => Some(SmartSpecKind::Git),
            "tar" => Some(SmartSpecKind::Tar),
            "zip" => Some(SmartSpecKind::Zip),
            "local" => Some(SmartSpecKind::Local),
            _ => None,
        }
    }

    /// Infer the archive/git kind of a URL from its suffix (defaults to git).
    fn from_url(url: &str) -> SmartSpecKind {
        let lower = url.to_ascii_lowercase();
        if lower.ends_with(".tar")
            || lower.ends_with(".tar.gz")
            || lower.ends_with(".tgz")
            || lower.ends_with(".tar.zst")
            || lower.ends_with(".tzst")
            || lower.ends_with(".tar.xz")
            || lower.ends_with(".txz")
        {
            SmartSpecKind::Tar
        } else if lower.ends_with(".zip") {
            SmartSpecKind::Zip
        } else {
            SmartSpecKind::Git
        }
    }
}

impl SmartSpec {
    /// Interpret `args.spec` (+ the relevant flags) into a typed [`Source`].
    ///
    /// Order of disambiguation: explicit `git|tar|zip|local <locator>` →
    /// `./`/`../` local → URL/scp-like by suffix → `owner/repo` GitHub shorthand
    /// (structural, never disk-dependent) → existing on-disk bare directory →
    /// error. A slash-bearing local path must be written `./owner/repo` so it is
    /// never silently confused with shorthand.
    pub fn parse(args: &AddArgs) -> Result<Self> {
        // Explicit form: `add {git|tar|zip|local} <locator>`.
        if args.spec.len() >= 2 {
            if let Some(kind) = SmartSpecKind::from_keyword(&args.spec[0]) {
                let locator = args.spec[1].clone();
                return Self::build(args, kind, locator);
            }
        }
        if args.spec.len() != 1 {
            return Err(SkmError::general(three_part(
                "error: could not interpret spec",
                "expected a single spec, or an explicit `git|tar|zip|local <locator>`",
                "Use an explicit subcommand form, e.g. `skm add git <url>`.",
            )));
        }
        let raw = args.spec[0].clone();

        // ./ or ../ → local.
        if raw.starts_with("./") || raw.starts_with("../") {
            return Self::build(args, SmartSpecKind::Local, raw);
        }

        // URL scheme or scp-like ssh → by suffix. An inline @ref only makes
        // sense for git; on an archive URL it is almost certainly a mistake, so
        // reject it explicitly here rather than letting `build` blame `--ref`.
        let is_url = raw.contains("://") || raw.starts_with("git@");
        if is_url {
            let (locator, inline_ref) = split_inline_ref(&raw);
            let kind = SmartSpecKind::from_url(&locator);
            if kind != SmartSpecKind::Git && inline_ref.is_some() {
                return Err(SkmError::general(
                    "error: an inline @ref is only valid for a git source",
                ));
            }
            let a = clone_args_with_ref(args, inline_ref)?;
            return Self::build(&a, kind, locator);
        }

        // owner/repo GitHub shorthand → git. Classified purely structurally (no
        // disk probe) so the same spec resolves identically on every machine; a
        // local directory that happens to share the name must be written
        // `./owner/repo`.
        if is_owner_repo(&raw) {
            let (locator, inline_ref) = split_inline_ref(&raw);
            let url = format!("https://github.com/{locator}");
            let a = clone_args_with_ref(args, inline_ref)?;
            return Self::build(&a, SmartSpecKind::Git, url);
        }

        // Exists on disk → local (must be a directory).
        if Path::new(&raw).is_dir() {
            return Self::build(args, SmartSpecKind::Local, raw);
        }
        if Path::new(&raw).exists() {
            return Err(SkmError::general(
                "local source must be a directory, not a file",
            ));
        }

        Err(SkmError::general(three_part(
            &format!("error: could not determine source type for '{raw}'"),
            "the spec is not a path, URL, or owner/repo",
            "Use an explicit subcommand, e.g. `skm add git <url>` or `skm add local <path>`.",
        )))
    }

    fn build(args: &AddArgs, kind: SmartSpecKind, locator: String) -> Result<Self> {
        // Validate flag/source compatibility.
        if args.ref_.is_some() && kind != SmartSpecKind::Git {
            return Err(SkmError::general(
                "error: --ref is only valid for a git source",
            ));
        }
        if args.sha256.is_some() && !matches!(kind, SmartSpecKind::Tar | SmartSpecKind::Zip) {
            return Err(SkmError::general(
                "error: --sha256 is only valid for a zip or tar source",
            ));
        }
        if args.subdir.is_some() && kind == SmartSpecKind::Local {
            return Err(SkmError::general(
                "error: --subdir is not valid for a local source",
            ));
        }

        // Normalize the subdir field (git/zip/tar) for safety up front.
        if let Some(sd) = &args.subdir {
            validate_source_subdir(sd)?;
        }

        let source = match kind {
            SmartSpecKind::Git => Source::Git {
                repo: locator,
                ref_: args.ref_.clone(),
                subdir: args.subdir.clone(),
            },
            SmartSpecKind::Tar => Source::Tar {
                url: locator,
                subdir: args.subdir.clone(),
                sha256: args.sha256.clone(),
            },
            SmartSpecKind::Zip => Source::Zip {
                url: locator,
                subdir: args.subdir.clone(),
                sha256: args.sha256.clone(),
            },
            SmartSpecKind::Local => Source::Local { path: locator },
        };
        Ok(SmartSpec { source, kind })
    }

    /// Determine the skill name: explicit `--name`, else inferred from the
    /// locator (or `--subdir`) with non-`[a-zA-Z0-9_-]` characters folded to `-`.
    pub fn infer_name(&self, args: &AddArgs) -> Result<String> {
        if let Some(n) = &args.name {
            if !manifest::valid_name(n) {
                return Err(SkmError::general(format!(
                    "error: invalid --name '{n}'; allowed characters: [a-zA-Z0-9_-]"
                )));
            }
            return Ok(n.clone());
        }

        let raw = if let Some(sd) = &args.subdir {
            last_segment(sd)
        } else {
            match (&self.kind, &self.source) {
                (SmartSpecKind::Git, Source::Git { repo, .. }) => {
                    let trimmed = repo.trim_end_matches('/').trim_end_matches(".git");
                    last_segment(trimmed)
                }
                (SmartSpecKind::Tar, Source::Tar { url, .. }) => {
                    strip_archive_ext(&last_segment(url))
                }
                (SmartSpecKind::Zip, Source::Zip { url, .. }) => {
                    last_segment(url).trim_end_matches(".zip").to_string()
                }
                (SmartSpecKind::Local, Source::Local { path }) => last_segment(path),
                _ => String::new(),
            }
        };

        let cleaned = clean_name(&raw);
        if cleaned.is_empty() || !manifest::valid_name(&cleaned) {
            return Err(SkmError::general(three_part(
                "error: could not infer a valid skill name",
                &format!("derived '{raw}' is not a valid name"),
                "Pass an explicit --name.",
            )));
        }
        Ok(cleaned)
    }
}

/// Carry an inline `@ref` into the args, checking for conflicts with `--ref`.
fn clone_args_with_ref(args: &AddArgs, inline_ref: Option<String>) -> Result<AddArgs> {
    let mut a = args.clone();
    if let Some(ir) = inline_ref {
        match &a.ref_ {
            Some(cli_ref) if cli_ref != &ir => {
                return Err(SkmError::general(format!(
                    "error: conflicting refs: '@{ir}' in spec vs '--ref {cli_ref}'; specify only one"
                )));
            }
            _ => a.ref_ = Some(ir),
        }
    }
    Ok(a)
}

fn is_owner_repo(s: &str) -> bool {
    // Strip an inline @ref before the structural check.
    let core = s.split('@').next().unwrap_or(s);
    let Some((owner, repo)) = core.split_once('/') else {
        return false;
    };
    // Exactly one '/', both parts non-empty, scp-like ':' excluded. Only the
    // *owner* must be dot-free (a real GitHub owner has no dots), so a bare host
    // like `example.com/x` is not mistaken for shorthand — while a dotted *repo*
    // name such as `user/user.github.io` still is.
    !owner.is_empty()
        && !owner.contains('.')
        && !repo.is_empty()
        && !repo.contains('/')
        && !core.contains(':')
}

/// Split a trailing `@ref` off a git-like spec.
fn split_inline_ref(raw: &str) -> (String, Option<String>) {
    // Avoid treating the ssh "git@host" '@' as a ref separator: only split on an
    // '@' that appears after the last '/'.
    if let Some(slash) = raw.rfind('/') {
        if let Some(at) = raw[slash..].find('@') {
            let at_abs = slash + at;
            let locator = raw[..at_abs].to_string();
            let r = raw[at_abs + 1..].to_string();
            if !r.is_empty() {
                return (locator, Some(r));
            }
        }
    } else if let Some(at) = raw.rfind('@') {
        if !raw.starts_with("git@") {
            return (raw[..at].to_string(), Some(raw[at + 1..].to_string()));
        }
    }
    (raw.to_string(), None)
}

fn last_segment(s: &str) -> String {
    s.trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(s)
        .to_string()
}

fn strip_archive_ext(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    for ext in [
        ".tar.gz", ".tar.zst", ".tar.xz", ".tgz", ".tzst", ".txz", ".tar",
    ] {
        if lower.ends_with(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s.to_string()
}

fn clean_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `AddArgs` for parsing tests: only the fields the spec parser
    /// reads matter; the rest are inert.
    fn args(spec: &[&str]) -> AddArgs {
        AddArgs {
            global: false,
            spec: spec.iter().map(|s| s.to_string()).collect(),
            subdir: None,
            ref_: None,
            name: None,
            sha256: None,
            agent: Vec::new(),
            no_sync: false,
            force: false,
        }
    }

    // ---- low-level helpers ----

    #[test]
    fn split_inline_ref_protects_ssh_host() {
        // The '@' in git@host is not a ref separator.
        assert_eq!(
            split_inline_ref("git@github.com:owner/repo.git"),
            ("git@github.com:owner/repo.git".to_string(), None)
        );
    }

    #[test]
    fn split_inline_ref_after_last_slash() {
        assert_eq!(
            split_inline_ref("https://github.com/owner/repo@v1.2.3"),
            (
                "https://github.com/owner/repo".to_string(),
                Some("v1.2.3".to_string())
            )
        );
        // An '@' before the last slash (e.g. ssh userinfo) is not a ref.
        assert_eq!(
            split_inline_ref("git@github.com:owner/repo@main"),
            (
                "git@github.com:owner/repo".to_string(),
                Some("main".to_string())
            )
        );
    }

    #[test]
    fn split_inline_ref_bare_owner_repo() {
        assert_eq!(
            split_inline_ref("owner/repo@dev"),
            ("owner/repo".to_string(), Some("dev".to_string()))
        );
    }

    #[test]
    fn split_inline_ref_empty_ref_is_ignored() {
        assert_eq!(
            split_inline_ref("owner/repo@"),
            ("owner/repo@".to_string(), None)
        );
    }

    #[test]
    fn is_owner_repo_recognizes_shorthand() {
        assert!(is_owner_repo("owner/repo"));
        assert!(is_owner_repo("owner/repo@v1")); // ref is split off before the check
        // A dotted *repo* name is still shorthand (only the owner must be
        // dot-free); regression for `user/user.github.io`.
        assert!(is_owner_repo("user/user.github.io"));
    }

    #[test]
    fn is_owner_repo_rejects_non_shorthand() {
        assert!(!is_owner_repo("owner/sub/repo")); // two slashes
        assert!(!is_owner_repo("host.com/repo")); // contains '.'
        assert!(!is_owner_repo("/abs/path")); // leading '/'
        assert!(!is_owner_repo("git@host:repo")); // contains ':'
        assert!(!is_owner_repo("single"));
    }

    #[test]
    fn from_url_by_suffix() {
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.tar.gz"),
            SmartSpecKind::Tar
        ));
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.TGZ"),
            SmartSpecKind::Tar
        )); // case-insensitive
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.tar.zst"),
            SmartSpecKind::Tar
        ));
        // Short compressed-tar suffixes the tar backend accepts must classify
        // as Tar too (else they would fall through to git).
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.tzst"),
            SmartSpecKind::Tar
        ));
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.txz"),
            SmartSpecKind::Tar
        ));
        assert!(matches!(
            SmartSpecKind::from_url("https://x/a.zip"),
            SmartSpecKind::Zip
        ));
        assert!(matches!(
            SmartSpecKind::from_url("https://github.com/o/r"),
            SmartSpecKind::Git
        ));
    }

    #[test]
    fn strip_archive_ext_handles_compound_extensions() {
        assert_eq!(strip_archive_ext("pkg.tar.gz"), "pkg");
        assert_eq!(strip_archive_ext("pkg.tar.zst"), "pkg");
        assert_eq!(strip_archive_ext("pkg.tgz"), "pkg");
        assert_eq!(strip_archive_ext("pkg.tar"), "pkg");
        assert_eq!(strip_archive_ext("plain"), "plain");
    }

    #[test]
    fn clean_name_folds_and_trims() {
        assert_eq!(clean_name("my skill!"), "my-skill");
        assert_eq!(clean_name("--edge--"), "edge");
        assert_eq!(clean_name("keep_under-dash"), "keep_under-dash");
        assert_eq!(clean_name("!!!"), "");
    }

    #[test]
    fn last_segment_splits_on_slash_and_colon() {
        assert_eq!(last_segment("a/b/c"), "c");
        assert_eq!(last_segment("git@host:owner/repo"), "repo");
        assert_eq!(last_segment("trailing/slash/"), "slash");
    }

    // ---- Spec::parse ----

    #[test]
    fn parse_explicit_kinds() {
        let p = SmartSpec::parse(&args(&["git", "https://github.com/o/r"])).unwrap();
        assert!(matches!(p.source, Source::Git { .. }));

        let p = SmartSpec::parse(&args(&["zip", "https://x/a.zip"])).unwrap();
        assert!(matches!(p.source, Source::Zip { .. }));

        let p = SmartSpec::parse(&args(&["local", "./some/dir"])).unwrap();
        assert!(matches!(p.source, Source::Local { .. }));
    }

    #[test]
    fn parse_owner_repo_becomes_github_git() {
        let p = SmartSpec::parse(&args(&["owner/repo"])).unwrap();
        match p.source {
            Source::Git { repo, ref_, .. } => {
                assert_eq!(repo, "https://github.com/owner/repo");
                assert_eq!(ref_, None);
            }
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn parse_owner_repo_with_inline_ref() {
        let p = SmartSpec::parse(&args(&["owner/repo@v2"])).unwrap();
        match p.source {
            Source::Git { repo, ref_, .. } => {
                assert_eq!(repo, "https://github.com/owner/repo");
                assert_eq!(ref_, Some("v2".to_string()));
            }
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn parse_url_archive_by_suffix() {
        assert!(matches!(
            SmartSpec::parse(&args(&["https://x/a.tar.gz"]))
                .unwrap()
                .source,
            Source::Tar { .. }
        ));
        assert!(matches!(
            SmartSpec::parse(&args(&["https://x/a.zip"]))
                .unwrap()
                .source,
            Source::Zip { .. }
        ));
    }

    #[test]
    fn parse_inline_ref_on_archive_url_errors_clearly() {
        // An @ref past the last slash of an archive URL is a mistake; the error
        // must name the @ref, not the unrelated `--ref` flag.
        let err = match SmartSpec::parse(&args(&["https://x/a.zip@v1"])) {
            Err(e) => e,
            Ok(_) => panic!("expected an error for an @ref on an archive URL"),
        };
        assert!(err.to_string().contains("inline @ref"), "got: {err}");
    }

    #[test]
    fn parse_relative_path_is_local() {
        assert!(matches!(
            SmartSpec::parse(&args(&["./dir"])).unwrap().source,
            Source::Local { .. }
        ));
        assert!(matches!(
            SmartSpec::parse(&args(&["../dir"])).unwrap().source,
            Source::Local { .. }
        ));
    }

    #[test]
    fn parse_unknown_spec_errors() {
        assert!(SmartSpec::parse(&args(&["not-a-source"])).is_err());
        assert!(SmartSpec::parse(&args(&[])).is_err());
    }

    #[test]
    fn inline_ref_conflicts_with_cli_ref() {
        let mut a = args(&["owner/repo@v1"]);
        a.ref_ = Some("v2".to_string());
        assert!(SmartSpec::parse(&a).is_err());

        // Same ref via both channels is fine.
        let mut a = args(&["owner/repo@v1"]);
        a.ref_ = Some("v1".to_string());
        assert!(SmartSpec::parse(&a).is_ok());
    }

    // ---- build flag-compatibility guards ----

    #[test]
    fn ref_only_valid_for_git() {
        let mut a = args(&["zip", "https://x/a.zip"]);
        a.ref_ = Some("v1".to_string());
        assert!(SmartSpec::parse(&a).is_err());
    }

    #[test]
    fn sha256_only_valid_for_archive() {
        let mut a = args(&["git", "https://github.com/o/r"]);
        a.sha256 = Some("deadbeef".to_string());
        assert!(SmartSpec::parse(&a).is_err());

        let mut a = args(&["tar", "https://x/a.tar.gz"]);
        a.sha256 = Some("deadbeef".to_string());
        assert!(SmartSpec::parse(&a).is_ok());
    }

    #[test]
    fn path_not_valid_for_local() {
        let mut a = args(&["local", "./dir"]);
        a.subdir = Some("sub".to_string());
        assert!(SmartSpec::parse(&a).is_err());
    }

    // ---- infer_name ----

    #[test]
    fn infer_name_explicit_is_validated() {
        let p = SmartSpec::parse(&args(&["owner/repo"])).unwrap();
        let mut a = args(&["owner/repo"]);
        a.name = Some("custom_name".to_string());
        assert_eq!(p.infer_name(&a).unwrap(), "custom_name");

        a.name = Some("bad name!".to_string());
        assert!(p.infer_name(&a).is_err());
    }

    #[test]
    fn infer_name_from_git_repo_strips_dot_git() {
        let a = args(&["git", "https://github.com/owner/cool-repo.git"]);
        let p = SmartSpec::parse(&a).unwrap();
        assert_eq!(p.infer_name(&a).unwrap(), "cool-repo");
    }

    #[test]
    fn infer_name_from_tar_url_strips_extension() {
        let a = args(&["tar", "https://x/my-skill.tar.gz"]);
        let p = SmartSpec::parse(&a).unwrap();
        assert_eq!(p.infer_name(&a).unwrap(), "my-skill");
    }

    #[test]
    fn infer_name_prefers_path_segment() {
        let mut a = args(&["git", "https://github.com/owner/repo"]);
        a.subdir = Some("skills/the-one".to_string());
        let p = SmartSpec::parse(&a).unwrap();
        assert_eq!(p.infer_name(&a).unwrap(), "the-one");
    }
}
