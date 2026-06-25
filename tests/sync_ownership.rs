//! Ownership & prune during sync: managed-extra prune vs foreign preservation,
//! two-pass ghost convergence, the foreign guard (identical re-claims, different
//! aborts), per-agent pruning, and the non-interactive prune-requires---yes gate.

mod support;
use support::*;

fn local_manifest(env: &Env, name: &str, agents: &[&str]) {
    env.create_local_skill(name, &[]);
    Manifest::v1()
        .default_agents(agents)
        .skill(Skill::local(name, &format!("./vendor/{name}")))
        .write_to(&env.manifest_path());
}

#[test]
fn ghost_only_sync_reports_cleanup() {
    // A sync that *only* drops an orphan lock entry (no install/repair/prune)
    // must not be silent: regression guard for the empty-summary case.
    let env = Env::new();
    local_manifest(&env, "m", &["agents"]);
    env.skm().arg("sync").assert().success();

    // Make `m` a managed extra (lock keeps it; manifest drops it)...
    env.skm()
        .args(["remove", "m", "--no-sync"])
        .assert()
        .success();
    // ...then delete its dir so there's nothing to prune — only the now-orphan
    // lock entry remains for ghost cleanup.
    std::fs::remove_dir_all(env.agents_root().join("m")).unwrap();

    env.skm()
        .args(["sync", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Cleaned 1 stale lock entry"));
    assert!(
        !env.lock_text().contains("name = \"m\""),
        "orphan lock entry should be gone; got:\n{}",
        env.lock_text()
    );
}

#[test]
fn prune_removes_managed_extra_keeps_foreign() {
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    env.skm().arg("sync").assert().success();

    // Create a foreign symlink and a foreign dir in the skills root.
    let root = env.agents_root();
    std::os::unix::fs::symlink(env.project.join("vendor/rev"), root.join("linky")).unwrap();
    write_file(&root.join("manualskill/SKILL.md"), "# manual\n", false);

    // Remove rev from the manifest, but keep lock+disk (managed extra).
    env.skm()
        .args(["remove", "rev", "--no-sync"])
        .assert()
        .success();
    env.skm().args(["sync", "--yes"]).assert().success();

    // Managed extra pruned; foreign untouched.
    assert!(!root.join("rev").exists(), "managed extra pruned");
    assert!(root.join("linky").exists(), "foreign symlink survives");
    assert!(root.join("manualskill").exists(), "foreign dir survives");
}

#[test]
fn two_pass_convergence_multi_agent() {
    let env = Env::new();
    local_manifest(&env, "rev", &["agents", "codex"]);
    env.skm().arg("sync").assert().success();
    assert!(env.agents_root().join("rev").is_dir());
    assert!(env.codex_root().join("rev").is_dir());

    // Entirely remove from the manifest (defer to sync).
    env.skm()
        .args(["remove", "rev", "--no-sync"])
        .assert()
        .success();

    // Pass one: prune disk in both roots; lock entry retained.
    env.skm().args(["sync", "--yes"]).assert().success();
    assert!(!env.agents_root().join("rev").exists());
    assert!(!env.codex_root().join("rev").exists());
    assert!(env.lock_text().contains("rev"), "lock retained pass 1");

    // Pass two: no residue → ghost-clean the lock entry.
    env.skm().args(["sync", "--yes"]).assert().success();
    assert!(!env.lock_text().contains("rev"), "lock ghost-cleaned");
}

#[test]
fn generic_agents_target_lands_in_dot_agents_skills() {
    // The tool-agnostic "agents" target deploys to ~/.agents/skills (here, <project>/.agents/skills).
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    env.skm().arg("sync").assert().success();
    assert!(env.agents_root().join("rev").is_dir());
    assert!(!env.claude_root().join("rev").exists());
    assert!(!env.codex_root().join("rev").exists());
}

#[test]
fn foreign_guard_identical_allows_different_aborts() {
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    // Lock + deploy once so we know the content.
    env.skm().arg("sync").assert().success();
    let content = read(&env.agents_root().join("rev/SKILL.md"));

    // Simulate lock loss + identical foreign dir already present.
    std::fs::remove_file(env.lock_path()).unwrap();
    // (dir already exists with identical content) → default sync re-claims.
    env.skm().arg("sync").assert().success();

    // Now make the foreign dir differ and drop the lock again.
    std::fs::remove_file(env.lock_path()).unwrap();
    write_file(
        &env.agents_root().join("rev/SKILL.md"),
        "DIFFERENT\n",
        false,
    );
    env.skm().arg("sync").assert().failure();
    // The foreign dir must be left untouched on abort (never overwritten).
    assert_eq!(
        read(&env.agents_root().join("rev/SKILL.md")),
        "DIFFERENT\n",
        "foreign content must be preserved when sync aborts"
    );
    let _ = content;
}

#[test]
fn no_prune_keeps_extra_and_does_not_report_a_prune() {
    // Regression: `--no-prune` must keep managed extras AND must not print a
    // false "Pruned N managed extra(s)" summary (the plan listed removals that
    // never happened). --no-prune.
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    env.skm().arg("sync").assert().success();
    env.skm()
        .args(["remove", "rev", "--no-sync"])
        .assert()
        .success();

    let out = env.skm().args(["sync", "--no-prune"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("Pruned"),
        "--no-prune must not report a prune; got: {stdout}"
    );
    assert!(
        env.agents_root().join("rev").exists(),
        "managed extra must be kept under --no-prune"
    );

    // Dry-run with --no-prune must likewise omit REMOVE and report no changes.
    let dry = env
        .skm()
        .args(["sync", "--dry-run", "--no-prune"])
        .output()
        .unwrap();
    assert_eq!(
        dry.status.code(),
        Some(0),
        "no pending changes under --no-prune"
    );
    assert!(
        !String::from_utf8_lossy(&dry.stdout).contains("REMOVE"),
        "dry-run --no-prune must not list REMOVE"
    );
}

#[test]
fn removing_an_agent_prunes_only_that_root() {
    // Dropping an agent from `agents` turns the old directory under
    // that root into a managed extra, pruned on sync; the kept agent survives.
    let env = Env::new();
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["agents", "codex"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());
    env.skm().args(["sync", "--yes"]).assert().success();
    assert!(env.agents_root().join("rev").is_dir());
    assert!(env.codex_root().join("rev").is_dir());

    // Drop codex from the agents.
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());
    env.skm().args(["sync", "--yes"]).assert().success();

    assert!(
        env.agents_root().join("rev").is_dir(),
        "kept agent survives"
    );
    assert!(
        !env.codex_root().join("rev").exists(),
        "removed agent's root pruned"
    );
}

#[test]
fn foreign_guard_symlink_aborts() {
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    env.skm().arg("sync").assert().success();

    std::fs::remove_file(env.lock_path()).unwrap();
    // Replace installed dir with one containing a symlink → foreign + unhashable.
    std::fs::remove_dir_all(env.agents_root().join("rev")).unwrap();
    let d = env.agents_root().join("rev");
    write_file(&d.join("SKILL.md"), "# rev\n", false);
    std::os::unix::fs::symlink("SKILL.md", d.join("link")).unwrap();
    let out = env.skm().arg("sync").output().unwrap();
    assert!(
        !out.status.success(),
        "sync must abort on unhashable foreign dir"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    // An I/O error while hashing (e.g. symlink) must not be conflated with a
    // content-mismatch: the message must mention "cannot inspect", not
    // "different content".
    assert!(
        stderr.contains("cannot inspect"),
        "expected 'cannot inspect' for unhashable directory, got: {stderr}"
    );
    assert!(
        !stderr.contains("different content"),
        "must not report 'different content' when hashing itself failed: {stderr}"
    );
}

#[test]
fn prune_without_yes_is_an_error_in_non_interactive_mode() {
    // A sync that needs to prune, with no --yes and no TTY, must error
    // rather than prune silently — and leave the managed extra in place.
    let env = Env::new();
    local_manifest(&env, "rev", &["agents"]);
    env.skm().arg("sync").assert().success();
    env.skm()
        .args(["remove", "rev", "--no-sync"])
        .assert()
        .success();

    env.skm()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("prune requires --yes"));
    assert!(
        env.agents_root().join("rev").exists(),
        "managed extra must survive a refused prune"
    );
}

#[test]
fn remove_only_deletes_from_declared_agents() {
    // Skill declared with agents=["claude"] → deploy only touches .claude/skills/.
    // A same-named directory in .codex/skills/ is foreign and must survive remove.
    let env = Env::new();
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["claude"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    // Only .claude was deployed to.
    assert!(env.claude_root().join("rev").is_dir());
    assert!(!env.codex_root().join("rev").exists());
    assert!(!env.agents_root().join("rev").exists());

    // Simulate an unrelated same-named dir in the codex root.
    std::fs::create_dir_all(env.codex_root().join("rev")).unwrap();
    std::fs::write(env.codex_root().join("rev/SKILL.md"), "# UNRELATED\n").unwrap();

    // Remove the skill.
    env.skm().args(["remove", "rev"]).assert().success();

    // The managed dir under .claude must be gone.
    assert!(
        !env.claude_root().join("rev").exists(),
        "managed dir under declared agent must be removed"
    );
    // The unrelated dir under .codex must survive.
    assert!(
        env.codex_root().join("rev").is_dir(),
        "unrelated same-named dir in non-declared root must be preserved"
    );
    assert_eq!(
        read(&env.codex_root().join("rev/SKILL.md")),
        "# UNRELATED\n",
        "foreign content must be intact"
    );
}
