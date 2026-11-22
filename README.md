# Skill Sync

A Rust library for discovering skills in a Git registry, comparing them with a local skills directory, syncing selected skills, and pushing local changes to the registry's `main` branch. It does not provide a CLI.

```rust
use skill_sync::{SkillRegistry, SyncOutcome};

fn main() -> Result<(), skill_sync::Error> {
	let registry = SkillRegistry::with_default_registry("/path/to/local/skills");
	let _skills = registry.list()?;
	let _exists = registry.exists_locally("rust")?;
	let _differs = registry.is_different("rust")?;
	let outcome = registry.sync("rust", false)?;
	assert_eq!(outcome, SyncOutcome::Synced);
	Ok(())
}
```

`SkillRegistry::new(remote_url, local_skills_dir)` selects a different Git registry. Use `sync_many` for selected names or `sync_all` for every immediate directory under the remote `skills/` folder. With `overwrite = false`, differing local folders are left untouched and reported as `SkippedExisting`; matching folders return `Unchanged`. A missing local folder is always synced.

`is_different` returns `true` when the skill is missing locally or its contents differ. Comparisons use BLAKE3 over sorted paths, file contents, and executable bits on Unix.

`update` commits the local skill and pushes directly to `main`, using the configured Git author identity. `update_with_identity` accepts an explicit author name and email. GitHub HTTPS pushes can use `GITHUB_TOKEN` or `GH_TOKEN`; SSH remotes use the SSH agent.

