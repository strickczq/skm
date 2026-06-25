//! `skm cache` subcommand: `cache dir` prints the (env-overridden) cache path,
//! and `cache clean` wipes source artifacts while preserving the flock files.

mod support;
use support::*;

#[test]
fn cache_dir_prints_cache_path() {
    // `skm cache dir`: prints the (env-overridden) global cache path.
    let env = Env::new();
    env.skm()
        .args(["cache", "dir"])
        .assert()
        .success()
        .stdout(predicates::str::contains(env.cache.display().to_string()));
}

#[test]
fn cache_clean_preserves_flocks_dir() {
    // `cache clean` must never delete `<cache>/flocks/`. Deleting a
    // live scope lock file unlinks its inode, so a re-open obtains a fresh inode
    // whose flock no longer mutually-excludes the prior holder — breaking
    // single-scope mutual exclusion.
    let env = Env::new();
    env.create_local_skill("rev", &[]);
    Manifest::v1()
        .default_agents(&["agents"])
        .skill(Skill::local("rev", "./vendor/rev"))
        .write_to(&env.manifest_path());

    // A write command acquires the scope flock, creating <cache>/flocks/<sha>.flock.
    env.skm().arg("sync").assert().success();

    let flocks = env.cache.join("flocks");
    assert!(flocks.is_dir(), "scope flock dir should exist after sync");
    let before: Vec<_> = std::fs::read_dir(&flocks)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(!before.is_empty(), "expected at least one scope flock file");

    // cache clean wipes source artifacts but must leave the lock files intact.
    env.skm().args(["cache", "clean"]).assert().success();

    assert!(flocks.is_dir(), "flocks/ must survive cache clean");
    let after: Vec<_> = std::fs::read_dir(&flocks)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before, after, "scope flock files must survive cache clean");

    // The cache lock file itself is also preserved.
    assert!(
        env.cache.join(".cache.lock").is_file(),
        ".cache.lock must survive cache clean"
    );
}
