//! Black-box coverage of the global (`-g`) scope. The rest of the
//! suite only exercises project scope; this verifies the global manifest/lock
//! live under `<skm_config>` and skills deploy to `~/.agents/skills`, with the
//! project scope left untouched (I1: distinct roots per scope).

mod support;
use support::*;

use std::path::PathBuf;

fn global_agents_root(env: &Env) -> PathBuf {
    env.home.join(".agents").join("skills")
}

#[test]
fn global_add_deploys_to_home_and_uses_config_manifest() {
    let env = Env::new();
    // A source directory outside any skills_root.
    let src = env.project.join("gsrc");
    write_file(&src.join("SKILL.md"), "# gs\n", false);

    env.skm().args(["init", "-g"]).assert().success();
    assert!(
        env.config.join("skm.toml").is_file(),
        "global manifest must live under <skm_config>"
    );

    env.skm()
        .args([
            "add",
            "-g",
            "local",
            &src.to_string_lossy(),
            "--name",
            "gs",
            "--agent",
            "agents",
        ])
        .assert()
        .success();

    // Deployed to the global root, lock written under config.
    assert!(
        global_agents_root(&env).join("gs/SKILL.md").is_file(),
        "global skill should land in ~/.agents/skills"
    );
    assert!(
        env.config.join("skm.lock").is_file(),
        "global lock under config"
    );

    // Project scope is entirely untouched (no implicit project manifest).
    assert!(!env.manifest_path().exists(), "no project skm.toml created");
    assert!(
        !env.agents_root().join("gs").exists(),
        "not in project root"
    );

    // `status -g` is clean.
    env.skm().args(["status", "-g"]).assert().success();
}

#[test]
fn global_and_project_same_name_are_independent() {
    // Global and project may share a skill name; distinct roots, no
    // interference (I1).
    let env = Env::new();

    // Global skill `dup`.
    let gsrc = env.project.join("gsrc");
    write_file(&gsrc.join("SKILL.md"), "# global\n", false);
    env.skm().args(["init", "-g"]).assert().success();
    env.skm()
        .args([
            "add",
            "-g",
            "local",
            &gsrc.to_string_lossy(),
            "--name",
            "dup",
            "--agent",
            "agents",
        ])
        .assert()
        .success();

    // Project skill `dup` with different content.
    env.create_local_skill("dup", &[("marker.txt", "project\n", false)]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("dup", "./vendor/dup"))
        .write_to(&env.manifest_path());
    env.skm().arg("sync").assert().success();

    // Both exist in their own roots with their own content.
    assert_eq!(
        read(&global_agents_root(&env).join("dup/SKILL.md")),
        "# global\n"
    );
    assert!(env.agents_root().join("dup/marker.txt").is_file());

    // Removing the project one leaves the global one intact.
    env.skm().args(["remove", "dup"]).assert().success();
    assert!(!env.agents_root().join("dup").exists());
    assert!(
        global_agents_root(&env).join("dup").is_dir(),
        "global skill must survive a project-scope remove"
    );
}
