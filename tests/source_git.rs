//! Git source: ref→commit pinning, --upgrade branch following (with the
//! default-branch-drift abort), path normalization, exec-bit hashing (H1),
//! and the symlink-in-tree rejection.

mod support;
use support::*;

#[test]
fn git_ref_to_commit_and_frozen() {
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("docx/SKILL.md", "# docx\n", false)
        .file("docx/x.txt", "y\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("docx", &repo.url()).subdir("docx"))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    env.assert_installed("docx");

    // Lock records a 40-hex resolved_commit.
    let lock = env.lock_text();
    assert!(lock.contains("resolved_commit"));
    assert!(lock.contains("type = \"git\""));

    // Delete + frozen → byte-for-byte recovery, no false drift.
    std::fs::remove_dir_all(env.agents_root().join("docx")).unwrap();
    env.skm().args(["sync", "--frozen"]).assert().success();
    env.assert_clean();
}

#[test]
fn invariant_h1_exec_bit_no_false_drift() {
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .file("run.sh", "#!/bin/sh\necho hi\n", true) // 100755
        .file("data.txt", "plain\n", false) // 100644
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    // Immediately status → installed (no false drift): proves H1.
    env.assert_clean();

    // +x correctly landed.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(env.agents_root().join("s/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0, "exec bit landed");

    // Frozen stays consistent.
    env.skm().args(["sync", "--frozen"]).assert().success();
    env.assert_clean();
}

#[test]
fn git_upgrade_follows_branch() {
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# v1\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    let lock1 = env.lock_text();

    // New commit on the default branch.
    repo.commit_file("SKILL.md", "# v2 updated\n", "v2");

    // Without --upgrade: identity unchanged → lock stays.
    env.skm().arg("lock").assert().success();
    assert_eq!(lock1, env.lock_text(), "respects lock w/o upgrade");

    // With --upgrade: re-resolves to the new HEAD.
    env.skm().args(["lock", "--upgrade"]).assert().success();
    assert_ne!(lock1, env.lock_text(), "upgrade re-resolves");
}

#[test]
fn update_upgrades_then_deploys_in_one_step() {
    // `skm update` is the one-shot of `lock --upgrade` + `sync`: it re-resolves
    // the mutable ref AND lands the new content on disk.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# v1\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    let lock1 = env.lock_text();
    assert_eq!(
        std::fs::read_to_string(env.agents_root().join("s/SKILL.md")).unwrap(),
        "# v1\n"
    );

    // New commit on the default branch.
    repo.commit_file("SKILL.md", "# v2 updated\n", "v2");

    // `update` re-resolves to the new HEAD and deploys it in one step.
    env.skm().arg("update").assert().success();
    assert_ne!(lock1, env.lock_text(), "update re-resolves the lock");
    assert_eq!(
        std::fs::read_to_string(env.agents_root().join("s/SKILL.md")).unwrap(),
        "# v2 updated\n",
        "update lands the new content"
    );
    env.assert_clean();
}

#[test]
fn update_named_unknown_skill_errors() {
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    env.skm()
        .args(["update", "nope"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot upgrade 'nope'"));
}

#[test]
fn lock_announces_network_resolve_then_quiet_when_cached() {
    // The resolve step prints a progress line ("Resolving <name> …") to stderr
    // when it must hit the network; a re-lock that reuses the cache stays quiet
    // (re_resolve == false, so no announcement).
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm()
        .arg("lock")
        .assert()
        .success()
        .stderr(predicates::str::contains("Resolving s (git"));

    // Second lock: identity matches, nothing re-resolves → no progress line.
    let out = env.skm().arg("lock").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Resolving"),
        "cached re-lock must not announce a resolve; got: {stderr}"
    );
}

#[test]
fn lock_prints_resolved_version() {
    // `skm lock` lists each newly-pinned entry with its resolved commit.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());
    let head = repo.head();
    env.skm()
        .arg("lock")
        .assert()
        .success()
        .stdout(predicates::str::contains("s → git"))
        .stdout(predicates::str::contains(&head[..12]));
}

#[test]
fn git_missing_skill_md_hints_specific_path() {
    // Top-level-directory pitfall: when the archive (here a git tree) has
    // exactly one top-level directory containing SKILL.md and the manifest
    // omits `subdir`, the error must suggest the precise `--subdir <name>` value.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("docx/SKILL.md", "# d\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("d", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm()
        .arg("lock")
        .assert()
        .failure()
        .stderr(predicates::str::contains("--subdir docx"));
}

#[test]
fn git_path_dot_prefix_no_resolve_churn() {
    // Git path is normalized — `./docx` and `docx` produce the same lock
    // entry and identity_matches returns true (no spurious re-resolve).
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("docx/SKILL.md", "# d\n", false)
        .commit("init");

    // First sync with `subdir = "docx"`.
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("d", &repo.url()).subdir("docx"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();
    let lock1 = env.lock_text();
    assert!(
        lock1.contains("subdir = \"docx\""),
        "lock should record normalized subdir; got:\n{lock1}"
    );
    assert!(
        !lock1.contains("subdir = \"./docx\""),
        "lock must not carry the ./ prefix; got:\n{lock1}"
    );

    // Rewrite manifest with `./docx` — identity must stay the same.
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("d", &repo.url()).subdir("./docx"))
        .write_to(&env.manifest_path());
    env.skm().arg("lock").assert().success();
    let lock2 = env.lock_text();
    assert_eq!(lock1, lock2, "./docx vs docx should not relock");
}

#[test]
fn git_uppercase_commit_sha_rejected_as_ref() {
    // Form detection: a commit SHA is `[0-9a-f]{40}`. An uppercase hex
    // string is NOT a commit and must be treated as a branch/tag name, which
    // will fail to resolve in this local repo with a clear network-style error.
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    let upper = repo.head().to_ascii_uppercase();
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()).ref_(&upper))
        .write_to(&env.manifest_path());

    // The uppercase form is not a commit pin → we ls-remote for it as a
    // branch/tag → not found → network-class error.
    env.skm()
        .arg("lock")
        .assert()
        .failure()
        .stderr(predicates::str::contains("not found in remote"));
}

#[test]
fn git_default_branch_drift_aborts_on_upgrade() {
    // With `ref` omitted, `--upgrade` follows the remote default branch, but
    // if that branch *changed* it must abort (never silently switch content).
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    // Discover the current default branch from the lock and switch HEAD away.
    let cur = env
        .lock_text()
        .lines()
        .find(|l| l.contains("resolved_ref"))
        .and_then(|l| l.split('"').nth(1))
        .unwrap()
        .to_string();
    let other = if cur == "master" { "main" } else { "master" };
    git(repo.path(), &["branch", other]);
    git(
        repo.path(),
        &["symbolic-ref", "HEAD", &format!("refs/heads/{other}")],
    );

    env.skm()
        .args(["lock", "--upgrade"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("remote default branch changed"));
}

#[test]
fn git_symlink_in_tree_is_rejected() {
    // A symlink inside the materialized content tree is intercepted at the
    // materialize stage (git archive carries it as a 120000 entry).
    let env = Env::new();
    let repo = GitRepo::init(&env, "repo")
        .file("SKILL.md", "# s\n", false)
        .commit("init");
    std::os::unix::fs::symlink("SKILL.md", repo.path().join("link")).unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "link"]);

    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::git("s", &repo.url()))
        .write_to(&env.manifest_path());

    env.skm()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("symlink not allowed"));
}
