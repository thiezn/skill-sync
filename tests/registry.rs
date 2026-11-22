use std::fs;
use std::path::{Path, PathBuf};

use git2::build::RepoBuilder;
use git2::{IndexAddOption, Repository, Signature};
use skill_sync::{SkillRegistry, SkillRegistrySet, SyncOutcome, UpdateOutcome};
use tempfile::TempDir;

fn create_registry(root: &Path) -> PathBuf {
    create_registry_with_skills(root, &[("alpha", "alpha v1"), ("beta", "beta v1")])
}

fn create_registry_with_skills(root: &Path, skills: &[(&str, &str)]) -> PathBuf {
    let source = root.join("source");
    for (skill, content) in skills {
        let directory = source.join("skills").join(skill);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("SKILL.md"), content).unwrap();
    }

    let repository = Repository::init(&source).unwrap();
    let mut index = repository.index().unwrap();
    index
        .add_all(["skills"], IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let signature = Signature::now("Registry Test", "registry@example.com").unwrap();
    repository
        .commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            "Initial registry",
            &tree,
            &[],
        )
        .unwrap();
    repository.set_head("refs/heads/main").unwrap();
    drop(tree);
    drop(index);
    drop(repository);

    let remote = root.join("registry.git");
    let mut builder = RepoBuilder::new();
    builder.bare(true);
    builder.clone(source.to_str().unwrap(), &remote).unwrap();
    let bare_repository = Repository::open_bare(&remote).unwrap();
    bare_repository.set_head("refs/heads/main").unwrap();
    remote
}

#[test]
fn registry_set_routes_skills_and_rejects_duplicate_names() {
    let temporary = TempDir::new().unwrap();
    let first_remote =
        create_registry_with_skills(&temporary.path().join("first"), &[("alpha", "alpha v1")]);
    let second_remote =
        create_registry_with_skills(&temporary.path().join("second"), &[("beta", "beta v1")]);
    let local_skills = temporary.path().join("local-skills");
    let first = SkillRegistry::new_with_cache_dir(
        first_remote.to_str().unwrap(),
        &local_skills,
        temporary.path().join("first-cache"),
    );
    let second = SkillRegistry::new_with_cache_dir(
        second_remote.to_str().unwrap(),
        &local_skills,
        temporary.path().join("second-cache"),
    );
    let registries = SkillRegistrySet::from_registries([first.clone(), second]);

    assert_eq!(
        registries
            .list()
            .unwrap()
            .into_iter()
            .map(|skill| skill.name)
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    assert_eq!(
        registries.sync_many(&["beta", "alpha"], false).unwrap(),
        [
            ("alpha".to_owned(), SyncOutcome::Synced),
            ("beta".to_owned(), SyncOutcome::Synced)
        ]
    );

    fs::write(local_skills.join("beta/SKILL.md"), "beta v2").unwrap();
    assert_eq!(
        registries
            .update_with_identity("beta", "Registry Test", "registry@example.com")
            .unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(latest_skill_content(&second_remote, "beta"), "beta v2");

    let duplicate = SkillRegistry::new_with_cache_dir(
        first_remote.to_str().unwrap(),
        &local_skills,
        temporary.path().join("duplicate-cache"),
    );
    let duplicate_registries = SkillRegistrySet::from_registries([first, duplicate]);
    let error = duplicate_registries.list().unwrap_err().to_string();
    assert!(error.contains("Duplicate skill name 'alpha'"));
    assert!(error.contains(first_remote.to_str().unwrap()));
}

fn latest_skill_content(repository_path: &Path, skill_name: &str) -> String {
    let repository = Repository::open_bare(repository_path).unwrap();
    let commit = repository
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let skills = commit
        .tree()
        .unwrap()
        .get_name("skills")
        .unwrap()
        .to_object(&repository)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let skill = skills
        .get_name(skill_name)
        .unwrap()
        .to_object(&repository)
        .unwrap()
        .peel_to_tree()
        .unwrap();
    let blob = skill
        .get_name("SKILL.md")
        .unwrap()
        .to_object(&repository)
        .unwrap()
        .peel_to_blob()
        .unwrap();
    String::from_utf8(blob.content().to_vec()).unwrap()
}

fn add_remote_skill(remote: &Path, working_directory: &Path, name: &str, content: &str) {
    let repository = Repository::clone(remote.to_str().unwrap(), working_directory).unwrap();
    let skill_directory = working_directory.join("skills").join(name);
    fs::create_dir_all(&skill_directory).unwrap();
    fs::write(skill_directory.join("SKILL.md"), content).unwrap();

    let mut index = repository.index().unwrap();
    index
        .add_all([format!("skills/{name}")], IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("Registry Test", "registry@example.com").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Add remote skill",
            &tree,
            &[&parent],
        )
        .unwrap();

    let mut remote = repository.find_remote("origin").unwrap();
    remote
        .push(&["refs/heads/main:refs/heads/main"], None)
        .unwrap();
}

#[test]
fn lists_compares_and_syncs_skills() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry_dir = temporary.path().join("registry-cache");
    let registry =
        SkillRegistry::new_with_cache_dir(remote.to_str().unwrap(), &local_skills, &registry_dir);

    let names = registry
        .list()
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["alpha", "beta"]);
    let cached_repository = Repository::open(&registry_dir).unwrap();
    assert!(cached_repository
        .find_reference("refs/remotes/origin/main")
        .is_ok());
    assert_eq!(
        cached_repository
            .config()
            .unwrap()
            .get_string("remote.origin.fetch")
            .unwrap(),
        "+refs/heads/main:refs/remotes/origin/main"
    );
    assert!(registry_dir.is_dir());
    assert!(!registry.exists_locally("alpha").unwrap());
    assert!(registry.is_different("alpha").unwrap());

    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);
    assert!(registry.exists_locally("alpha").unwrap());
    assert!(!registry.is_different("alpha").unwrap());

    fs::write(local_skills.join("alpha/SKILL.md"), "local change").unwrap();
    assert_eq!(
        registry.sync("alpha", false).unwrap(),
        SyncOutcome::SkippedExisting
    );
    assert_eq!(registry.sync("alpha", true).unwrap(), SyncOutcome::Synced);
    assert_eq!(
        fs::read_to_string(local_skills.join("alpha/SKILL.md")).unwrap(),
        "alpha v1"
    );

    let outcomes = registry.sync_many(&["alpha", "beta"], false).unwrap();
    assert_eq!(outcomes[0], ("alpha".to_owned(), SyncOutcome::Unchanged));
    assert_eq!(outcomes[1], ("beta".to_owned(), SyncOutcome::Synced));

    let outcomes = registry.sync_all(false).unwrap();
    assert!(outcomes
        .iter()
        .all(|(_, outcome)| *outcome == SyncOutcome::Unchanged));
}

#[test]
fn updates_and_pushes_skill_to_main() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry_dir = temporary.path().join("registry-cache");
    let registry =
        SkillRegistry::new_with_cache_dir(remote.to_str().unwrap(), &local_skills, &registry_dir);
    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);

    let cached_repository = Repository::open(&registry_dir).unwrap();
    let mut config = cached_repository.config().unwrap();
    config.set_str("user.name", "Skill Sync Test").unwrap();
    config.set_str("user.email", "sync@example.com").unwrap();

    fs::write(local_skills.join("alpha/SKILL.md"), "alpha v2").unwrap();
    assert_eq!(registry.update("alpha").unwrap(), UpdateOutcome::Updated);
    assert_eq!(latest_skill_content(&remote, "alpha"), "alpha v2");
    assert_eq!(registry.update("alpha").unwrap(), UpdateOutcome::Unchanged);
}

#[test]
fn update_fetches_new_remote_commit_before_pushing() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry_dir = temporary.path().join("registry-cache");
    let registry =
        SkillRegistry::new_with_cache_dir(remote.to_str().unwrap(), &local_skills, &registry_dir);
    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);

    let cached_repository = Repository::open(&registry_dir).unwrap();
    let mut config = cached_repository.config().unwrap();
    config.set_str("user.name", "Skill Sync Test").unwrap();
    config.set_str("user.email", "sync@example.com").unwrap();
    drop(config);

    add_remote_skill(
        &remote,
        &temporary.path().join("upstream-update"),
        "gamma",
        "gamma v1",
    );
    fs::write(local_skills.join("alpha/SKILL.md"), "alpha v2").unwrap();

    assert_eq!(registry.update("alpha").unwrap(), UpdateOutcome::Updated);
    assert_eq!(latest_skill_content(&remote, "alpha"), "alpha v2");
    assert_eq!(latest_skill_content(&remote, "gamma"), "gamma v1");
}

#[test]
fn update_retry_preserves_and_pushes_an_existing_local_commit() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry_dir = temporary.path().join("registry-cache");
    let registry =
        SkillRegistry::new_with_cache_dir(remote.to_str().unwrap(), &local_skills, &registry_dir);
    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);

    let cached_repository = Repository::open(&registry_dir).unwrap();
    let cached_skill = registry_dir.join("skills/alpha/SKILL.md");
    fs::write(&cached_skill, "alpha v2").unwrap();
    let mut index = cached_repository.index().unwrap();
    index
        .add_all(["skills"], IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = cached_repository.find_tree(tree_id).unwrap();
    let parent = cached_repository.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("Skill Sync Test", "sync@example.com").unwrap();
    cached_repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Update skill 'alpha'",
            &tree,
            &[&parent],
        )
        .unwrap();

    fs::write(local_skills.join("alpha/SKILL.md"), "alpha v2").unwrap();
    assert_eq!(registry.list().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(cached_skill).unwrap(), "alpha v2");
    assert_eq!(registry.update("alpha").unwrap(), UpdateOutcome::Updated);
    assert_eq!(latest_skill_content(&remote, "alpha"), "alpha v2");
}

#[test]
fn list_detects_diverged_cache_after_fetching_remote() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry_dir = temporary.path().join("registry-cache");
    let registry =
        SkillRegistry::new_with_cache_dir(remote.to_str().unwrap(), &local_skills, &registry_dir);
    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);

    let cached_repository = Repository::open(&registry_dir).unwrap();
    fs::write(registry_dir.join("skills/alpha/SKILL.md"), "alpha local v2").unwrap();
    let mut index = cached_repository.index().unwrap();
    index
        .add_all(["skills"], IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = cached_repository.find_tree(tree_id).unwrap();
    let parent = cached_repository.head().unwrap().peel_to_commit().unwrap();
    let signature = Signature::now("Skill Sync Test", "sync@example.com").unwrap();
    cached_repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Update skill 'alpha'",
            &tree,
            &[&parent],
        )
        .unwrap();

    add_remote_skill(
        &remote,
        &temporary.path().join("upstream-update"),
        "gamma",
        "gamma v1",
    );

    let error = registry.list().unwrap_err().to_string();
    assert!(error.contains("Cached main and origin/main have diverged"));
}
