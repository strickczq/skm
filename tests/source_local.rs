//! Local source: the full lock→sync→drift→repair chain, frozen reproducibility,
//! path-spelling normalization (no resolve churn), and the inside-skills_root /
//! dangling-symlink guards.

mod support;
use support::*;

#[test]
fn local_full_chain_and_drift() {
    let env = Env::new();
    env.create_local_skill("rev", &[("a.txt", "hello\n", false)]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    env.assert_installed("rev");

    // Installed → status 0.
    env.assert_clean();

    // Modify the installed dir → drift (exit 2).
    write_file(&env.agents_root().join("rev/a.txt"), "tampered\n", false);
    env.assert_drift();

    // Normal sync repairs.
    env.skm().arg("sync").assert().success();
    env.assert_clean();
}

#[test]
fn lock_does_not_deploy_sync_does() {
    let env = Env::new();
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    env.skm().arg("lock").assert().success();
    assert!(env.lock_path().is_file(), "lock written");
    assert!(
        !env.agents_root().join("rev").exists(),
        "lock must not deploy"
    );

    env.skm().arg("sync").assert().success();
    assert!(env.agents_root().join("rev").is_dir(), "sync deploys");
}

#[test]
fn reproducible_frozen_after_delete() {
    let env = Env::new();
    env.create_local_skill("rev", &[("data.bin", "abc123\n", false)]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    let lock1 = env.lock_text();

    std::fs::remove_dir_all(env.agents_root().join("rev")).unwrap();
    env.skm().args(["sync", "--frozen"]).assert().success();
    env.assert_clean();
    // Lock unchanged.
    assert_eq!(lock1, env.lock_text());
}

#[test]
fn missing_skill_md_is_always_an_error() {
    // A content tree without SKILL.md always fails — there is no escape hatch.
    let env = Env::new();
    let src = env.project.join("vendor/nofile");
    write_file(&src.join("notes.txt"), "x\n", false);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("nofile", "./vendor/nofile"))
        .write_to(&env.manifest_path());

    env.skm().arg("lock").assert().failure().code(1);
}

#[test]
fn local_inside_skills_root_errors() {
    let env = Env::new();
    // Point local at a path inside .agents/skills.
    let inside = env.project.join(".agents/skills/evil");
    write_file(&inside.join("SKILL.md"), "# evil\n", false);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("evil", "./.agents/skills/evil"))
        .write_to(&env.manifest_path());

    env.skm()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("lies inside a skills_root"));
}

#[test]
fn local_equivalent_spellings_no_resolve_churn() {
    // `./vendor/rev` and `../<proj>/vendor/rev` normalize to the same
    // canonical path. Switching the manifest between the two spellings must
    // not re-resolve (identity_matches must be true).
    let env = Env::new();
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    env.skm().arg("sync").assert().success();
    let hash1 = env.lock_sha256("rev");

    // Re-spell as `../<project_dir_name>/vendor/rev` — same canonical path.
    let proj_name = env
        .project
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", &format!("../{proj_name}/vendor/rev")))
        .write_to(&env.manifest_path());

    // Sync must not error (would error under the old strict normalize_local).
    env.skm().arg("sync").assert().success();

    // content_sha256 must be unchanged (no content drift, no false relock).
    assert_eq!(
        env.lock_sha256("rev"),
        hash1,
        "content_sha256 should be stable across equivalent local-path spellings"
    );
}

#[test]
fn local_dangling_symlink_error_distinct() {
    let env = Env::new();
    // ./vendor is a symlink to a missing target.
    std::os::unix::fs::symlink(
        env.project.join("missing-target"),
        env.project.join("vendor"),
    )
    .unwrap();
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    env.skm()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicates::str::contains("symlink target missing"));
}
