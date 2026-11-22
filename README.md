# Skill Sync

A Rust library for discovering skills in a Git registry, comparing them with a local skills directory, syncing selected skills, and pushing local changes to the registry's `main` branch. It does not provide a CLI.

```rust
use skill_sync::{SkillRegistry, SyncOutcome};

fn main() -> Result<(), skill_sync::Error> {
	let registry = SkillRegistry::new("https://github.com/thiezn/skills.git", "/path/to/local/skills");
	let _skills = registry.list()?;
	let _exists = registry.exists_locally("rust")?;
	let _differs = registry.is_different("rust")?;
	let outcome = registry.sync("rust", false)?;
	assert_eq!(outcome, SyncOutcome::Synced);
	Ok(())
}
```

`SkillRegistry::new(remote_url, local_skills_dir)` requires an explicit Git registry URL. Use `sync_many` for selected names or `sync_all` for every immediate directory under the remote `skills/` folder. With `overwrite = false`, differing local folders are left untouched and reported as `SkippedExisting`; matching folders return `Unchanged`. A missing local folder is always synced.

Use `SkillRegistrySet` when working with multiple registries. It lists skills across all registries, rejects duplicate skill names, and routes sync and update operations to the registry that owns each skill:

```rust
use skill_sync::SkillRegistrySet;

let registries = SkillRegistrySet::new(
	["https://example.com/team-skills.git", "https://example.com/personal-skills.git"],
	"/path/to/local/skills",
);
let skills = registries.list()?;
registries.sync_all(false)?;
```

`is_different` returns `true` when the skill is missing locally or its contents differ. Comparisons use BLAKE3 over sorted paths, file contents, and executable bits on Unix.

Registry checkouts are cached under `~/.utl/skills/<repository-name>` and reused between operations. For example, the default `skills.git` registry is stored in `~/.utl/skills/skills`, while a `skill-sync.git` repository is stored in `~/.utl/skills/skill-sync`. If different repositories have the same name, a URL hash is appended to avoid collisions. Fetches retrieve only `main` with depth 1 for network remotes. Local path remotes use libgit2's local transport, which does not support shallow fetches.

In `~/.utl/config.toml`, set `[skills].repos` to explicitly configure one or more repositories used by `utl`:

```toml
[skills]
repos = ["https://github.com/thiezn/skills.git"]
```

The `utl skills list`, `sync`, and `update` commands accept repeatable `--repo URL` flags; when present, they replace the configured list for that invocation. No registry is selected implicitly. If two selected repositories contain a skill with the same name, the operation reports the duplicate and names both repositories.

`update` commits the local skill and pushes directly to `main`, using `git config` to resolve the effective `user.name` and `user.email`. `update_with_identity` accepts an explicit author name and email. Pushes use configured Git credential helpers first (such as macOS Keychain), then GitHub `GITHUB_TOKEN`/`GH_TOKEN` or the SSH agent as applicable.

