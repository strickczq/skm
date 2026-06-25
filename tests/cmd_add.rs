//! `skm add` contract: smart spec detection (path/URL/owner-repo, tar/zip
//! kinds, name inference), conflicting-flag guards, the home-pollution guard,
//! and resolve-first (an invalid source must never mutate the manifest).

mod support;
use support::*;

#[test]
fn add_local_infers_name_and_syncs() {
    let env = Env::new();
    env.create_local_skill("reviewer", &[]);
    env.skm().arg("init").assert().success();
    env.skm()
        .args(["add", "./vendor/reviewer", "--agent", "agents"])
        .assert()
        .success();
    env.assert_installed("reviewer");
    assert!(env.manifest_text().contains("reviewer"));
}

#[test]
fn add_help_shows_examples() {
    // The smart-spec shapes are discoverable from `add --help`.
    let env = Env::new();
    env.skm()
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Examples:"))
        .stdout(predicates::str::contains("owner/repo"));
}

#[test]
fn add_git_prints_locked_resolved_commit() {
    // `add` must surface the version it pinned (short commit), so the user
    // never has to open skm.lock to learn what got installed.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    env.skm().arg("init").assert().success();
    let head = repo.head();
    env.skm()
        .args(["add", &repo.url(), "--agent", "agents"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Locked 'repo'"))
        .stdout(predicates::str::contains(&head[..12]))
        // The deploy summary names the concrete agent + landing dir, not a
        // bare count, so `-g` users can confirm where it landed.
        .stdout(predicates::str::contains("Installed 'repo' → agents"))
        .stdout(predicates::str::contains("skills/repo"));
}

#[test]
fn add_ref_conflict_errors() {
    let env = Env::new();
    env.skm().arg("init").assert().success();
    env.skm()
        .args(["add", "anthropics/skills@v1.2", "--ref", "v2.0"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("conflicting refs"));
}

#[test]
fn add_sha256_for_git_errors() {
    let env = Env::new();
    env.skm().arg("init").assert().success();
    env.skm()
        .args(["add", "anthropics/skills", "--sha256", "abc"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--sha256 is only valid"));
}

#[test]
fn add_pollution_guard_in_home() {
    let env = Env::new();
    // CWD == HOME, no skm.toml, project (non -g) add must refuse.
    env.skm_in(&env.home)
        .arg("add")
        .arg("./whatever")
        .assert()
        .failure()
        .stderr(predicates::str::contains("refusing to create"));

    // With --force it proceeds past the guard (then fails on the missing dir,
    // but not with the pollution message).
    let out = env
        .skm_in(&env.home)
        .args(["add", "./whatever", "--force"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("refusing to create"));
}

#[test]
fn add_no_agents_without_defaults_errors() {
    let env = Env::new();
    // `add` without --agent must force an explicit choice.
    write_file(&env.manifest_path(), "version = 1\n", false);
    env.create_local_skill("x", &[]);
    env.skm()
        .arg("add")
        .arg("./vendor/x")
        .assert()
        .failure()
        .stderr(predicates::str::contains("no agents"))
        .stderr(predicates::str::contains("--agent agents|claude|codex"));
}

#[test]
fn add_after_init_without_agent_errors() {
    // A bare `add` after `init` must prompt for an explicit agent.
    let env = Env::new();
    env.create_local_skill("x", &[]);
    env.skm().arg("init").assert().success();
    env.skm()
        .args(["add", "./vendor/x"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("no agents"));
}

#[test]
fn re_add_same_spec_is_idempotent() {
    let env = Env::new();
    env.create_local_skill("reviewer", &[]);
    env.skm().arg("init").assert().success();
    env.skm()
        .args(["add", "./vendor/reviewer", "--agent", "agents"])
        .assert()
        .success();

    // Second add with the identical spec reconverges instead of erroring.
    env.skm()
        .args(["add", "./vendor/reviewer", "--agent", "agents"])
        .assert()
        .success()
        .stderr(predicates::str::contains("already in manifest"));
    env.assert_installed("reviewer");
    // Manifest still has exactly one entry for the name.
    assert_eq!(
        env.manifest_text().matches("name = \"reviewer\"").count(),
        1
    );
}

#[test]
fn re_add_different_spec_errors() {
    let env = Env::new();
    env.create_local_skill("a", &[]);
    env.create_local_skill("b", &[]);
    env.skm().arg("init").assert().success();
    // Land under an explicit name so both specs target the same skill name.
    env.skm()
        .args(["add", "./vendor/a", "--name", "tool", "--agent", "agents"])
        .assert()
        .success();
    // Re-adding 'tool' from a different path must refuse.
    env.skm()
        .args(["add", "./vendor/b", "--name", "tool", "--agent", "agents"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("different spec"));
}

/// Resolve-first: an invalid source must NOT mutate the manifest.
#[test]
fn add_invalid_source_does_not_mutate_manifest() {
    let env = Env::new();
    // Manifest with explicit [defaults].agents so the no-agents error
    // doesn't fire before we reach resolve.
    Manifest::v1()
        .default_agents(&["agents"])
        .write_to(&env.manifest_path());
    let manifest_before = env.manifest_text();

    // A non-existent local directory fails at resolve time.
    env.skm().args(["add", "./nonexistent"]).assert().failure();

    // Manifest must be byte-for-byte unchanged.
    assert_eq!(env.manifest_text(), manifest_before);
}

/// When git ls-remote fails with an auth-related error, the error message
/// should give a targeted hint about credential helpers instead of a generic
/// "check your network" message.
#[test]
fn add_auth_failure_shows_credential_hint() {
    let env = Env::new();
    Manifest::v1()
        .default_agents(&["agents"])
        .write_to(&env.manifest_path());

    // Plant a fake `git` that simulates an auth failure for ls-remote and
    // delegates everything else to the real git.
    let fake = FakeCmd::new("git", &which_git())
        .on_arg(
            "ls-remote",
            128,
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
        )
        .install(&env.home);

    let out = env
        .skm()
        .env("PATH", fake.path())
        .args(["add", "some/private-repo", "--subdir", "foo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("requires authentication"),
        "expected auth hint, got:\n{stderr}"
    );
    assert!(
        stderr.contains("credential helper"),
        "expected credential helper mention, got:\n{stderr}"
    );

    // Manifest must be untouched (resolve ran first, failed, nothing written).
    assert!(
        !env.manifest_text().contains("private-repo"),
        "manifest should not contain the failed entry"
    );
}

/// `owner/repo` shorthand is classified structurally, not by probing the cwd:
/// even when a directory of the same name exists, the spec still resolves as a
/// GitHub git source (so the same skm.toml behaves identically on every
/// machine). A local path that collides must be written `./owner/repo`.
#[test]
fn owner_repo_shorthand_ignores_same_named_local_dir() {
    let env = Env::new();
    Manifest::v1()
        .default_agents(&["agents"])
        .write_to(&env.manifest_path());

    // A real on-disk directory that shares the shorthand's name.
    write_file(
        &env.project.join("some/private-repo/SKILL.md"),
        "# local\n",
        false,
    );

    // Fake git that fails ls-remote with an auth error — proof we reached the
    // GitHub resolve path rather than treating the dir as a local source.
    let fake = FakeCmd::new("git", &which_git())
        .on_arg(
            "ls-remote",
            128,
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
        )
        .install(&env.home);

    let out = env
        .skm()
        .env("PATH", fake.path())
        .args(["add", "some/private-repo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("requires authentication"),
        "expected the git auth path (shorthand won), got:\n{stderr}"
    );
}

fn which_git() -> String {
    std::process::Command::new("which")
        .arg("git")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
        .unwrap_or_else(|| "/usr/bin/git".to_string())
}

#[test]
fn add_sha256_for_local_errors() {
    // --sha256 is only valid for zip/tar.
    let env = Env::new();
    env.create_local_skill("x", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .write_to(&env.manifest_path());
    env.skm()
        .args(["add", "./vendor/x", "--sha256", "abc", "--no-sync"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--sha256 is only valid"));
}

#[test]
fn add_undeterminable_spec_errors() {
    // Smart spec: a value that is neither path, URL, nor owner/repo errors
    // with a hint to use an explicit subcommand.
    let env = Env::new();
    Manifest::v1()
        .default_agents(&["agents"])
        .write_to(&env.manifest_path());
    env.skm()
        .args(["add", "weird:thing", "--no-sync"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("could not determine source type"));
}

#[test]
fn add_tar_url_detects_kind_and_infers_name() {
    // A `.tar.gz` URL → tar source; name inferred from the filename with the
    // archive suffix stripped.
    let env = Env::new();
    let tar = Archive::tar_gz().file("SKILL.md", "# u\n", false).build();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/unix-util.tar.gz")
        .with_body(tar)
        .create();
    let url = format!("{}/unix-util.tar.gz", server.url());
    write_file(
        &env.manifest_path(),
        "version = 1\n[defaults]\nagents = [\"claude-code\"]\n",
        false,
    );

    env.skm()
        .args(["add", &url, "--no-sync"])
        .assert()
        .success();
    let m = env.manifest_text();
    assert!(
        m.contains("name = \"unix-util\""),
        "name inference; got {m}"
    );
    assert!(m.contains("tar = "), "tar kind written; got {m}");
}

#[test]
fn add_zip_url_detects_kind_and_infers_name() {
    let env = Env::new();
    let zip = Archive::zip().file("SKILL.md", "# w\n", false).build();
    let mut server = mockito::Server::new();
    server.mock("GET", "/my-tool.zip").with_body(zip).create();
    let url = format!("{}/my-tool.zip", server.url());
    write_file(
        &env.manifest_path(),
        "version = 1\n[defaults]\nagents = [\"claude-code\"]\n",
        false,
    );

    env.skm()
        .args(["add", &url, "--no-sync"])
        .assert()
        .success();
    let m = env.manifest_text();
    assert!(m.contains("name = \"my-tool\""), "name inference; got {m}");
    assert!(m.contains("zip = "), "zip kind written; got {m}");
}
