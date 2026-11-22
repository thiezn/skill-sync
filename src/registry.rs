use std::env;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use git2::build::RepoBuilder;
use git2::{
    BranchType, Cred, CredentialType, FetchOptions, IndexAddOption, PushOptions, RemoteCallbacks,
    Repository, Signature,
};

use crate::{Error, Result};

pub const DEFAULT_REGISTRY_URL: &str = "https://github.com/thiezn/skills.git";

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
}

struct Checkout {
    _directory: tempfile::TempDir,
    path: PathBuf,
    repository: Repository,
}

impl SkillRegistry {
    pub fn new(remote_url: impl Into<String>, local_skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            remote_url: remote_url.into(),
            local_skills_dir: local_skills_dir.into(),
        }
    }

    pub fn with_default_registry(local_skills_dir: impl Into<PathBuf>) -> Self {
        Self::new(DEFAULT_REGISTRY_URL, local_skills_dir)
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
        let local_path = self.local_skills_dir.join(&skill_name);
        ensure_skill_directory(&local_path, &skill_name)?;

        let checkout = self.clone_registry()?;
        checkout_main(&checkout.repository)?;

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
            return Ok(UpdateOutcome::Unchanged);
        }

        let signature = match identity {
            Some((name, email)) => Signature::now(name, email)?,
            None => repository.signature().map_err(|error| {
                Error::with_source(
                    "Git author identity is missing; configure user.name and user.email",
                    error,
                )
            })?,
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

        let mut remote = repository.find_remote("origin")?;
        let mut options = PushOptions::new();
        options.remote_callbacks(remote_callbacks());
        remote.push(&["refs/heads/main:refs/heads/main"], Some(&mut options))?;

        Ok(UpdateOutcome::Updated)
    }

    fn local_skill_path(&self, skill_name: &str) -> Result<PathBuf> {
        Ok(self.local_skills_dir.join(validate_skill_name(skill_name)?))
    }

    fn clone_registry(&self) -> Result<Checkout> {
        let checkout_dir = tempfile::tempdir()?;
        let path = checkout_dir.path().join("registry");
        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(remote_callbacks());
        let mut builder = RepoBuilder::new();
        builder.fetch_options(fetch_options);
        let repository = builder.clone(&self.remote_url, &path)?;
        Ok(Checkout {
            _directory: checkout_dir,
            path,
            repository,
        })
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

fn checkout_main(repository: &Repository) -> Result<()> {
    let remote_main = repository
        .find_branch("origin/main", BranchType::Remote)
        .map_err(|error| Error::with_source("The registry does not have a main branch", error))?;
    let commit = remote_main.get().peel_to_commit()?;
    if repository.find_branch("main", BranchType::Local).is_err() {
        repository.branch("main", &commit, false)?;
    }
    repository.set_head("refs/heads/main")?;
    repository.checkout_head(None)?;
    Ok(())
}

fn remote_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(credentials);
    callbacks
}

fn credentials(
    _url: &str,
    username: Option<&str>,
    allowed: CredentialType,
) -> std::result::Result<Cred, git2::Error> {
    if allowed.contains(CredentialType::SSH_KEY) {
        return Cred::ssh_key_from_agent(username.unwrap_or("git"));
    }

    if allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
        && let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN"))
    {
        return Cred::userpass_plaintext(username.unwrap_or("x-access-token"), &token);
    }

    Cred::default()
}
