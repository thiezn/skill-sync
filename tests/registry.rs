use std::fs;
use std::path::{Path, PathBuf};

use git2::build::RepoBuilder;
use git2::{IndexAddOption, Repository, Signature};
use skill_sync::{SkillRegistry, SyncOutcome, UpdateOutcome};
use tempfile::TempDir;

fn create_registry(root: &Path) -> PathBuf {
    let source = root.join("source");
    for (skill, content) in [("alpha", "alpha v1"), ("beta", "beta v1")] {
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

#[test]
fn lists_compares_and_syncs_skills() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry = SkillRegistry::new(remote.to_str().unwrap(), &local_skills);

    let names = registry
        .list()
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["alpha", "beta"]);
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
    assert!(
        outcomes
            .iter()
            .all(|(_, outcome)| *outcome == SyncOutcome::Unchanged)
    );
}

#[test]
fn updates_and_pushes_skill_to_main() {
    let temporary = TempDir::new().unwrap();
    let remote = create_registry(temporary.path());
    let local_skills = temporary.path().join("local-skills");
    let registry = SkillRegistry::new(remote.to_str().unwrap(), &local_skills);
    assert_eq!(registry.sync("alpha", false).unwrap(), SyncOutcome::Synced);

    fs::write(local_skills.join("alpha/SKILL.md"), "alpha v2").unwrap();
    assert_eq!(
        registry
            .update_with_identity("alpha", "Skill Sync Test", "sync@example.com")
            .unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(latest_skill_content(&remote, "alpha"), "alpha v2");
    assert_eq!(
        registry
            .update_with_identity("alpha", "Skill Sync Test", "sync@example.com")
            .unwrap(),
        UpdateOutcome::Unchanged
    );
}
