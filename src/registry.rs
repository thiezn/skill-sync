use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use git2::{
    BranchType, Config, Cred, CredentialType, FetchOptions, IndexAddOption, PushOptions,
    RemoteCallbacks, Repository, Signature,
};

use crate::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skill {
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncOutcome {
    Synced,
    Unchanged,
    SkippedExisting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateOutcome {
    Updated,
    Unchanged,
}

#[derive(Clone, Debug)]
pub struct SkillRegistry {
    remote_url: String,
    local_skills_dir: PathBuf,
    registry_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct SkillRegistrySet {
    registries: Vec<(String, SkillRegistry)>,
}

struct Checkout {
    path: PathBuf,
    repository: Repository,
}

impl SkillRegistry {
    pub fn new(remote_url: impl Into<String>, local_skills_dir: impl Into<PathBuf>) -> Self {
        let remote_url = remote_url.into();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cache_root = home.join(".utl").join("skills");
        let registry_dir = registry_cache_dir(&cache_root, &remote_url);

        Self {
            remote_url,
            local_skills_dir: local_skills_dir.into(),
            registry_dir,
        }
    }

    pub fn new_with_cache_dir(
        remote_url: impl Into<String>,
        local_skills_dir: impl Into<PathBuf>,
        registry_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            remote_url: remote_url.into(),
            local_skills_dir: local_skills_dir.into(),
            registry_dir: registry_dir.into(),
        }
    }

    pub fn list(&self) -> Result<Vec<Skill>> {
        let checkout = self.clone_registry()?;
        list_checkout_skills(&checkout)
    }

    pub fn exists_locally(&self, skill_name: &str) -> Result<bool> {
        let local_path = self.local_skill_path(skill_name)?;
        Ok(local_path.is_dir())
    }

    pub fn is_different(&self, skill_name: &str) -> Result<bool> {
        let skill_name = validate_skill_name(skill_name)?;
        let checkout = self.clone_registry()?;
        let remote_path = checkout.path.join("skills").join(&skill_name);
        ensure_skill_directory(&remote_path, &skill_name)?;

        let local_path = self.local_skills_dir.join(&skill_name);
        if !local_path.is_dir() {
            return Ok(true);
        }

        Ok(directory_hash(&remote_path)? != directory_hash(&local_path)?)
    }

    pub fn sync(&self, skill_name: &str, overwrite: bool) -> Result<SyncOutcome> {
        let skill_name = skill_name.to_owned();
        self.sync_many(&[skill_name], overwrite)
            .map(|mut outcomes| outcomes.remove(0).1)
    }

    pub fn sync_many<S>(
        &self,
        skill_names: &[S],
        overwrite: bool,
    ) -> Result<Vec<(String, SyncOutcome)>>
    where
        S: AsRef<str>,
    {
        let checkout = self.clone_registry()?;
        skill_names
            .iter()
            .map(|skill_name| {
                let skill_name = validate_skill_name(skill_name.as_ref())?;
                let outcome = self.sync_from_checkout(&checkout, &skill_name, overwrite)?;
                Ok((skill_name, outcome))
            })
            .collect()
    }

    pub fn sync_all(&self, overwrite: bool) -> Result<Vec<(String, SyncOutcome)>> {
        let checkout = self.clone_registry()?;
        list_checkout_skills(&checkout)?
            .into_iter()
            .map(|skill| {
                let outcome = self.sync_from_checkout(&checkout, &skill.name, overwrite)?;
                Ok((skill.name, outcome))
            })
            .collect()
    }

    pub fn update(&self, skill_name: &str) -> Result<UpdateOutcome> {
        self.update_with_optional_identity(skill_name, None)
    }

    pub fn update_with_identity(
        &self,
        skill_name: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<UpdateOutcome> {
        self.update_with_optional_identity(skill_name, Some((author_name, author_email)))
    }

    fn update_with_optional_identity(
        &self,
        skill_name: &str,
        identity: Option<(&str, &str)>,
    ) -> Result<UpdateOutcome> {
        let skill_name = validate_skill_name(skill_name)?;
        let checkout = self.clone_registry()?;
        let local_path = self.local_skills_dir.join(&skill_name);
        if !local_path.is_dir() {
            return Err(Error::new(format!(
                "Local skill '{skill_name}' was not found in '{}'",
                self.local_skills_dir.display()
            )));
        }
        let remote_path = checkout.path.join("skills").join(&skill_name);
        fs::create_dir_all(
            remote_path.parent().ok_or_else(|| {
                Error::new(format!("Invalid remote path for skill '{skill_name}'"))
            })?,
        )?;
        replace_directory(&local_path, &remote_path)?;

        let repository = &checkout.repository;
        let mut index = repository.index()?;
        index.add_all(["skills"], IndexAddOption::DEFAULT, None)?;
        index.update_all(["skills"], None)?;
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = repository.find_tree(tree_oid)?;
        let parent = repository.head()?.peel_to_commit()?;
        if parent.tree_id() == tree_oid {
            let remote_main = repository
                .find_reference("refs/remotes/origin/main")?
                .peel_to_commit()?;
            if parent.id() == remote_main.id() {
                return Ok(UpdateOutcome::Unchanged);
            }
            push_main(repository)?;
            return Ok(UpdateOutcome::Updated);
        }

        let signature = match identity {
            Some((name, email)) => Signature::now(name, email)?,
            None => configured_signature(repository)?,
        };
        let message = format!("Update skill '{skill_name}'");
        repository.commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&parent],
        )?;

        push_main(repository)?;

        Ok(UpdateOutcome::Updated)
    }

    fn local_skill_path(&self, skill_name: &str) -> Result<PathBuf> {
        Ok(self.local_skills_dir.join(validate_skill_name(skill_name)?))
    }

    fn clone_registry(&self) -> Result<Checkout> {
        let registry_dir = self.resolved_registry_dir();
        let parent = registry_dir
            .parent()
            .ok_or_else(|| Error::new("Registry cache directory has no parent"))?;
        fs::create_dir_all(parent)?;

        let mut created = false;
        if !registry_dir.exists() {
            let staging = tempfile::Builder::new()
                .prefix(".skills-sync-")
                .tempdir_in(parent)?;
            let staging_path = staging.path().join("registry");
            let repository = Repository::init(&staging_path)?;
            repository.remote("origin", &self.remote_url)?;
            let mut config = repository.config()?;
            config.set_str(
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            )?;
            drop(config);
            fetch_main(&repository, &self.remote_url)?;
            checkout_main(&repository, false)?;
            drop(repository);
            if let Err(error) = fs::rename(&staging_path, &registry_dir) {
                if !registry_dir.exists() {
                    return Err(Error::from(error));
                }
            } else {
                created = true;
            }
        }

        let repository = Repository::open(&registry_dir)?;
        if repository.config()?.get_string("remote.origin.url")? != self.remote_url {
            return Err(Error::new(format!(
                "Registry cache at '{}' points to a different remote",
                registry_dir.display()
            )));
        }

        if !created {
            fetch_main(&repository, &self.remote_url)?;
        }
        let preserve_local_main = !created && local_main_is_ahead_of_remote(&repository)?;
        checkout_main(&repository, preserve_local_main)?;

        Ok(Checkout {
            path: registry_dir,
            repository,
        })
    }

    fn resolved_registry_dir(&self) -> PathBuf {
        if !self.registry_dir.exists()
            || registry_cache_matches(&self.registry_dir, &self.remote_url)
        {
            return self.registry_dir.clone();
        }

        let hash = blake3::hash(self.remote_url.as_bytes())
            .to_hex()
            .to_string();
        let name = self
            .registry_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository");
        self.registry_dir
            .with_file_name(format!("{name}-{}", &hash[..12]))
    }

    fn sync_from_checkout(
        &self,
        checkout: &Checkout,
        skill_name: &str,
        overwrite: bool,
    ) -> Result<SyncOutcome> {
        let remote_path = checkout.path.join("skills").join(skill_name);
        ensure_skill_directory(&remote_path, skill_name)?;
        let local_path = self.local_skills_dir.join(skill_name);

        if local_path.is_dir() {
            if directory_hash(&remote_path)? == directory_hash(&local_path)? {
                return Ok(SyncOutcome::Unchanged);
            }
            if !overwrite {
                return Ok(SyncOutcome::SkippedExisting);
            }
        }

        fs::create_dir_all(&self.local_skills_dir)?;
        replace_directory(&remote_path, &local_path)?;
        Ok(SyncOutcome::Synced)
    }
}

impl SkillRegistrySet {
    pub fn new<I, S>(remote_urls: I, local_skills_dir: impl Into<PathBuf>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let local_skills_dir = local_skills_dir.into();
        let registries = remote_urls
            .into_iter()
            .map(|remote_url| {
                let remote_url = remote_url.into();
                let registry = SkillRegistry::new(&remote_url, &local_skills_dir);
                (remote_url, registry)
            })
            .collect();
        Self { registries }
    }

    pub fn from_registries(registries: impl IntoIterator<Item = SkillRegistry>) -> Self {
        Self {
            registries: registries
                .into_iter()
                .map(|registry| (registry.remote_url.clone(), registry))
                .collect(),
        }
    }

    pub fn list(&self) -> Result<Vec<Skill>> {
        Ok(self
            .list_owned_skills()?
            .into_iter()
            .map(|(_, skill)| skill)
            .collect())
    }

    pub fn sync_many<S>(
        &self,
        skill_names: &[S],
        overwrite: bool,
    ) -> Result<Vec<(String, SyncOutcome)>>
    where
        S: AsRef<str>,
    {
        let owned_skills = self.list_owned_skills()?;
        let selected = skill_names
            .iter()
            .map(|skill_name| {
                let skill_name = skill_name.as_ref();
                owned_skills
                    .iter()
                    .find(|(_, skill)| skill.name == skill_name)
                    .map(|(index, skill)| (*index, skill.name.clone()))
                    .ok_or_else(|| {
                        Error::new(format!(
                            "Skill '{skill_name}' was not found in the selected repositories"
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        self.sync_owned_skills(&selected, overwrite)
    }

    pub fn sync_all(&self, overwrite: bool) -> Result<Vec<(String, SyncOutcome)>> {
        let selected = self
            .list_owned_skills()?
            .into_iter()
            .map(|(index, skill)| (index, skill.name))
            .collect::<Vec<_>>();
        self.sync_owned_skills(&selected, overwrite)
    }

    pub fn update(&self, skill_name: &str) -> Result<UpdateOutcome> {
        self.update_with_optional_identity(skill_name, None)
    }

    pub fn update_with_identity(
        &self,
        skill_name: &str,
        author_name: &str,
        author_email: &str,
    ) -> Result<UpdateOutcome> {
        self.update_with_optional_identity(skill_name, Some((author_name, author_email)))
    }

    fn update_with_optional_identity(
        &self,
        skill_name: &str,
        identity: Option<(&str, &str)>,
    ) -> Result<UpdateOutcome> {
        let owned_skills = self.list_owned_skills()?;
        let registry_index = owned_skills
            .iter()
            .find(|(_, skill)| skill.name == skill_name)
            .map(|(index, _)| *index)
            .or_else(|| (self.registries.len() == 1).then_some(0))
            .ok_or_else(|| {
                Error::new(format!(
                    "Skill '{skill_name}' was not found in the selected repositories; adding a skill requires selecting one repository"
                ))
            })?;
        let registry = &self.registries[registry_index].1;
        match identity {
            Some((name, email)) => registry.update_with_identity(skill_name, name, email),
            None => registry.update(skill_name),
        }
    }

    fn list_owned_skills(&self) -> Result<Vec<(usize, Skill)>> {
        let mut owners = std::collections::HashMap::<String, usize>::new();
        let mut skills = Vec::new();

        for (registry_index, (repository, registry)) in self.registries.iter().enumerate() {
            for skill in registry.list()? {
                if let Some(first_registry_index) = owners.get(&skill.name) {
                    return Err(Error::new(format!(
                        "Duplicate skill name '{}' found in repositories '{}' and '{}'",
                        skill.name, self.registries[*first_registry_index].0, repository
                    )));
                }
                owners.insert(skill.name.clone(), registry_index);
                skills.push((registry_index, skill));
            }
        }

        skills.sort_by(|left, right| left.1.name.cmp(&right.1.name));
        Ok(skills)
    }

    fn sync_owned_skills(
        &self,
        selected: &[(usize, String)],
        overwrite: bool,
    ) -> Result<Vec<(String, SyncOutcome)>> {
        let mut skills_by_registry = vec![Vec::new(); self.registries.len()];
        for (registry_index, skill_name) in selected {
            skills_by_registry[*registry_index].push(skill_name.clone());
        }

        let mut outcomes = Vec::new();
        for ((_, registry), skill_names) in self.registries.iter().zip(skills_by_registry) {
            if skill_names.is_empty() {
                continue;
            }
            outcomes.extend(registry.sync_many(&skill_names, overwrite)?);
        }
        Ok(outcomes)
    }
}

fn registry_cache_dir(cache_root: &Path, remote_url: &str) -> PathBuf {
    let remote_name = remote_url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repository");
    let remote_name = remote_name.strip_suffix(".git").unwrap_or(remote_name);
    let safe_name = remote_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let safe_name = safe_name.trim_matches('.');
    if safe_name.is_empty() {
        cache_root.join("repository")
    } else {
        cache_root.join(safe_name)
    }
}

fn registry_cache_matches(path: &Path, remote_url: &str) -> bool {
    Repository::open(path)
        .ok()
        .and_then(|repository| {
            repository
                .config()
                .ok()?
                .get_string("remote.origin.url")
                .ok()
        })
        .is_some_and(|cached_url| cached_url == remote_url)
}

fn validate_skill_name(skill_name: &str) -> Result<String> {
    let mut components = Path::new(skill_name).components();
    let is_single_name = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !skill_name.contains(['/', '\\']);
    if !is_single_name || skill_name == "." || skill_name == ".." {
        return Err(Error::new(format!("Invalid skill name '{skill_name}'")));
    }
    Ok(skill_name.to_owned())
}

fn ensure_skill_directory(path: &Path, skill_name: &str) -> Result<()> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "Skill '{skill_name}' was not found in the registry"
        )))
    }
}

fn list_checkout_skills(checkout: &Checkout) -> Result<Vec<Skill>> {
    let skills_dir = checkout.path.join("skills");
    if !skills_dir.is_dir() {
        return Err(Error::new(
            "The registry does not contain a skills directory",
        ));
    }

    let mut skills = Vec::new();
    for entry in fs::read_dir(skills_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            skills.push(Skill {
                name: entry.file_name().to_string_lossy().into_owned(),
            });
        }
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

fn directory_hash(path: &Path) -> Result<[u8; 32]> {
    if !path.is_dir() {
        return Err(Error::new(format!(
            "'{}' is not a directory",
            path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    hash_directory_contents(path, Path::new(""), &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}

fn hash_directory_contents(
    root: &Path,
    relative: &Path,
    hasher: &mut blake3::Hasher,
) -> Result<()> {
    let directory = root.join(relative);
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        let child_relative = relative.join(&name);
        let metadata = fs::symlink_metadata(entry.path())?;
        hash_path(hasher, &child_relative);

        if metadata.file_type().is_dir() {
            hasher.update(b"directory\0");
            hash_directory_contents(root, &child_relative, hasher)?;
        } else if metadata.file_type().is_file() {
            hasher.update(b"file\0");
            hasher.update(&metadata.len().to_le_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let executable = metadata.permissions().mode() & 0o111;
                hasher.update(&executable.to_le_bytes());
            }
            let mut file = fs::File::open(entry.path())?;
            let mut buffer = [0; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        } else if metadata.file_type().is_symlink() {
            hasher.update(b"symlink\0");
            hash_path(hasher, &fs::read_link(entry.path())?);
        } else {
            return Err(Error::new(format!(
                "Unsupported file type in skill folder: '{}'",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn hash_path(hasher: &mut blake3::Hasher, path: &Path) {
    let path = path.to_string_lossy();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path.as_bytes());
}

fn replace_directory(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| Error::new("Destination directory has no parent"))?;
    fs::create_dir_all(parent)?;
    let staging_root = tempfile::Builder::new()
        .prefix(".skill-sync-")
        .tempdir_in(parent)?;
    let staged = staging_root.path().join("new");
    copy_directory(source, &staged)?;

    let destination_exists = match fs::symlink_metadata(destination) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(Error::from(error)),
    };
    if !destination_exists {
        fs::rename(&staged, destination)?;
        return Ok(());
    }

    let backup = staging_root.path().join("previous");
    fs::rename(destination, &backup)?;
    if let Err(error) = fs::rename(&staged, destination) {
        let restore_error = fs::rename(&backup, destination).err();
        return match restore_error {
            Some(restore_error) => Err(Error::with_source(
                format!(
                    "Could not replace the skill folder, and restoring its backup also failed: {restore_error}"
                ),
                error,
            )),
            None => Err(Error::from(error)),
        };
    }
    if fs::symlink_metadata(&backup)?.file_type().is_dir() {
        fs::remove_dir_all(backup)?;
    } else {
        fs::remove_file(backup)?;
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = fs::symlink_metadata(&source_path)?.file_type();
        if file_type.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(source_path, destination_path)?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
        } else {
            return Err(Error::new(format!(
                "Unsupported file type in skill folder: '{}'",
                source_path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)?;
    let target_path = source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&target);
    if target_path.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(_source: &Path, _destination: &Path) -> Result<()> {
    Err(Error::new(
        "Symbolic links are not supported on this platform",
    ))
}

fn configured_signature(repository: &Repository) -> Result<Signature<'static>> {
    let config = repository.config()?;
    let identity = "Git author identity is missing; configure user.name and user.email";
    let name = config
        .get_string("user.name")
        .map_err(|_| Error::new(identity))?;
    let email = config
        .get_string("user.email")
        .map_err(|_| Error::new(identity))?;
    Signature::now(&name, &email).map_err(Into::into)
}

fn supports_shallow_fetch(remote_url: &str) -> bool {
    !remote_url.starts_with("file://") && !Path::new(remote_url).exists()
}

fn fetch_main(repository: &Repository, remote_url: &str) -> Result<()> {
    if repository.config()?.get_string("remote.origin.url")? != remote_url {
        return Err(Error::new("Registry cache points to a different remote"));
    }

    let mut remote = repository.find_remote("origin")?;
    let mut fetch_options = FetchOptions::new();
    if supports_shallow_fetch(remote_url) {
        fetch_options.depth(1);
    }
    fetch_options.remote_callbacks(remote_callbacks()?);
    remote.fetch(
        &["+refs/heads/main:refs/remotes/origin/main"],
        Some(&mut fetch_options),
        None,
    )?;
    Ok(())
}

fn local_main_is_ahead_of_remote(repository: &Repository) -> Result<bool> {
    let Ok(local_main) = repository.find_reference("refs/heads/main") else {
        return Ok(false);
    };
    let Ok(remote_main) = repository.find_reference("refs/remotes/origin/main") else {
        return Ok(false);
    };
    let local_id = local_main
        .target()
        .ok_or_else(|| Error::new("Cached main branch does not point to a commit"))?;
    let remote_id = remote_main
        .target()
        .ok_or_else(|| Error::new("Cached origin/main branch does not point to a commit"))?;

    if local_id == remote_id {
        return Ok(false);
    }
    if repository.graph_descendant_of(local_id, remote_id)? {
        return Ok(true);
    }
    if repository.graph_descendant_of(remote_id, local_id)? {
        return Ok(false);
    }

    Err(Error::new(
        "Cached main and origin/main have diverged; refusing to reset local commits",
    ))
}

fn checkout_main(repository: &Repository, preserve_local_main: bool) -> Result<()> {
    let remote_main = repository
        .find_branch("origin/main", BranchType::Remote)
        .map_err(|error| Error::with_source("The registry does not have a main branch", error))?;
    let commit = remote_main.get().peel_to_commit()?;
    if repository.find_branch("main", BranchType::Local).is_err() {
        repository.branch("main", &commit, false)?;
    }
    repository.set_head("refs/heads/main")?;
    if !preserve_local_main {
        repository.reset(commit.as_object(), git2::ResetType::Hard, None)?;
    }
    Ok(())
}

fn push_main(repository: &Repository) -> Result<()> {
    let mut remote = repository.find_remote("origin")?;
    let mut options = PushOptions::new();
    options.remote_callbacks(remote_callbacks()?);
    remote
        .push(&["refs/heads/main:refs/heads/main"], Some(&mut options))
        .map_err(|error| {
            Error::with_source(
                format!("Could not push skill updates to origin/main: {error}"),
                error,
            )
        })
}

fn remote_callbacks<'a>() -> Result<RemoteCallbacks<'a>> {
    let config = Config::open_default()?;
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username, allowed| {
        if let Ok(credential) = Cred::credential_helper(&config, url, username) {
            return Ok(credential);
        }

        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
            && let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN"))
        {
            return Cred::userpass_plaintext(username.unwrap_or("x-access-token"), &token);
        }

        if allowed.contains(CredentialType::SSH_KEY) {
            return Cred::ssh_key_from_agent(username.unwrap_or("git"));
        }

        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(username.unwrap_or("git"));
        }

        Cred::default()
    });
    Ok(callbacks)
}

#[cfg(test)]
mod tests {
    use super::registry_cache_dir;
    use std::path::Path;

    #[test]
    fn registry_cache_path_uses_repository_name() {
        assert_eq!(
            registry_cache_dir(
                Path::new("/home/user/.utl/skills"),
                "https://github.com/thiezn/skills.git"
            ),
            Path::new("/home/user/.utl/skills/skills")
        );
    }
}
