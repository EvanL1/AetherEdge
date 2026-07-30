//! Hardened Unix file layout for Gateway identity state.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aether_ports::GatewayIdentityError;
use fs2::FileExt as _;
use zeroize::Zeroizing;

const SEED_FILE_NAME: &str = "gateway-identity.seed";
const STATE_FILE_NAME: &str = "gateway-enrollment.json";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const MAX_STATE_BYTES: u64 = 16 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct StoredIdentityFiles {
    pub(crate) seed: Zeroizing<[u8; 32]>,
    pub(crate) state: Vec<u8>,
}

pub(crate) struct IdentityLayout {
    directory: PathBuf,
    parent: PathBuf,
    seed_path: PathBuf,
    state_path: PathBuf,
    lock_path: PathBuf,
    directory_name: String,
}

impl IdentityLayout {
    pub(crate) fn new(directory: &Path) -> Result<Self, GatewayIdentityError> {
        validate_configured_path(directory)?;
        let parent = directory
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or(GatewayIdentityError::InsecureStorage)?
            .to_path_buf();
        let directory_name = directory
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or(GatewayIdentityError::InsecureStorage)?
            .to_string();
        let lock_path = parent.join(format!(".{directory_name}.enrollment.lock"));
        Ok(Self {
            directory: directory.to_path_buf(),
            parent,
            seed_path: directory.join(SEED_FILE_NAME),
            state_path: directory.join(STATE_FILE_NAME),
            lock_path,
            directory_name,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn read_secret_identity(
        &self,
    ) -> Result<Option<StoredIdentityFiles>, GatewayIdentityError> {
        if !validate_existing_directory_chain(&self.directory, true)? {
            return Ok(None);
        }
        let seed = read_seed(&self.seed_path)?;
        let state = read_state(&self.state_path)?;
        Ok(Some(StoredIdentityFiles { seed, state }))
    }

    pub(crate) fn lock_for_write(&self) -> Result<ExclusiveIdentityLock, GatewayIdentityError> {
        ensure_secure_directory_chain(&self.parent)?;
        let file = open_or_create_lock(&self.lock_path)?;
        file.lock_exclusive()
            .map_err(|_| GatewayIdentityError::Unavailable)?;
        validate_open_file_and_path(&file, &self.lock_path)?;
        Ok(ExclusiveIdentityLock { file })
    }

    pub(crate) fn write_initial(
        &self,
        seed: &[u8; 32],
        state: &[u8],
    ) -> Result<(), GatewayIdentityError> {
        validate_state_size(state)?;
        match std::fs::symlink_metadata(&self.directory) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {},
            Ok(_) => return Err(GatewayIdentityError::Conflict),
            Err(_) => return Err(GatewayIdentityError::Unavailable),
        }

        let staging = self.unique_temporary_path(&self.parent);
        create_private_directory(&staging)?;
        let staging_seed = staging.join(SEED_FILE_NAME);
        let staging_state = staging.join(STATE_FILE_NAME);
        let result = (|| {
            write_new_private_file(&staging_seed, seed)?;
            write_new_private_file(&staging_state, state)?;
            sync_directory(&staging)?;
            match std::fs::symlink_metadata(&self.directory) {
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {},
                Ok(_) => return Err(GatewayIdentityError::Conflict),
                Err(_) => return Err(GatewayIdentityError::Unavailable),
            }
            std::fs::rename(&staging, &self.directory)
                .map_err(|_| GatewayIdentityError::Unavailable)?;
            sync_directory(&self.parent)
        })();
        if result.is_err() {
            cleanup_staging_directory(&staging, &staging_seed, &staging_state);
        }
        result
    }

    pub(crate) fn replace_state(&self, state: &[u8]) -> Result<(), GatewayIdentityError> {
        validate_state_size(state)?;
        if !validate_existing_directory_chain(&self.directory, true)? {
            return Err(GatewayIdentityError::CorruptState);
        }
        validate_existing_private_file(&self.state_path)?;

        let temporary = self.unique_temporary_path(&self.directory);
        let result = (|| {
            write_new_private_file(&temporary, state)?;
            validate_existing_private_file(&self.state_path)?;
            std::fs::rename(&temporary, &self.state_path)
                .map_err(|_| GatewayIdentityError::Unavailable)?;
            sync_directory(&self.directory)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    fn unique_temporary_path(&self, parent: &Path) -> PathBuf {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        parent.join(format!(
            ".{}.tmp-{}-{sequence}",
            self.directory_name,
            std::process::id()
        ))
    }
}

pub(crate) struct ExclusiveIdentityLock {
    file: File,
}

impl Drop for ExclusiveIdentityLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn validate_configured_path(path: &Path) -> Result<(), GatewayIdentityError> {
    #[cfg(not(unix))]
    {
        let _ = path;
        return Err(GatewayIdentityError::InsecureStorage);
    }
    #[cfg(unix)]
    {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(GatewayIdentityError::InsecureStorage);
        }
        for component in path.components() {
            if !matches!(component, Component::RootDir | Component::Normal(_)) {
                return Err(GatewayIdentityError::InsecureStorage);
            }
        }
        Ok(())
    }
}

fn validate_existing_directory_chain(
    path: &Path,
    exact_leaf_mode: bool,
) -> Result<bool, GatewayIdentityError> {
    let mut current = PathBuf::new();
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(name) => current.push(name),
            _ => return Err(GatewayIdentityError::InsecureStorage),
        }
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(GatewayIdentityError::Unavailable),
        };
        let is_leaf = components.peek().is_none();
        validate_directory_metadata(&metadata, exact_leaf_mode && is_leaf)?;
    }
    Ok(true)
}

fn ensure_secure_directory_chain(path: &Path) -> Result<(), GatewayIdentityError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(name) => current.push(name),
            _ => return Err(GatewayIdentityError::InsecureStorage),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => validate_directory_metadata(&metadata, false)?,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                create_private_directory(&current)?;
                if let Some(parent) = current.parent()
                    && !parent.as_os_str().is_empty()
                {
                    sync_directory(parent)?;
                }
            },
            Err(_) => return Err(GatewayIdentityError::Unavailable),
        }
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), GatewayIdentityError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let mut builder = std::fs::DirBuilder::new();
        match builder.mode(DIRECTORY_MODE).create(path) {
            Ok(()) => {},
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|_| GatewayIdentityError::Unavailable)?;
                return validate_directory_metadata(&metadata, true);
            },
            Err(_) => return Err(GatewayIdentityError::Unavailable),
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIRECTORY_MODE))
            .map_err(|_| GatewayIdentityError::Unavailable)?;
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| GatewayIdentityError::Unavailable)?;
        validate_directory_metadata(&metadata, true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(GatewayIdentityError::InsecureStorage)
    }
}

fn validate_directory_metadata(
    metadata: &std::fs::Metadata,
    exact_owner_only: bool,
) -> Result<(), GatewayIdentityError> {
    if !metadata.file_type().is_dir() {
        return Err(GatewayIdentityError::InsecureStorage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let effective_uid = unsafe { libc::geteuid() };
        let owner = metadata.uid();
        let mode = metadata.permissions().mode();
        if exact_owner_only {
            if owner != effective_uid || mode & 0o7777 != DIRECTORY_MODE {
                return Err(GatewayIdentityError::InsecureStorage);
            }
        } else if (owner != 0 && owner != effective_uid)
            || (mode & 0o022 != 0 && mode & 0o1000 == 0)
        {
            return Err(GatewayIdentityError::InsecureStorage);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = exact_owner_only;
        Err(GatewayIdentityError::InsecureStorage)
    }
}

fn open_or_create_lock(path: &Path) -> Result<File, GatewayIdentityError> {
    match open_existing_private_file(path, true)? {
        Some(file) => Ok(file),
        None => match create_new_private_file(path, true) {
            Ok(file) => {
                file.sync_all()
                    .map_err(|_| GatewayIdentityError::Unavailable)?;
                if let Some(parent) = path.parent() {
                    sync_directory(parent)?;
                }
                Ok(file)
            },
            Err(GatewayIdentityError::Conflict) => {
                open_existing_private_file(path, true)?.ok_or(GatewayIdentityError::Unavailable)
            },
            Err(error) => Err(error),
        },
    }
}

fn read_seed(path: &Path) -> Result<Zeroizing<[u8; 32]>, GatewayIdentityError> {
    let mut file =
        open_existing_private_file(path, false)?.ok_or(GatewayIdentityError::CorruptState)?;
    validate_seed_length(&file)?;
    let mut seed = Zeroizing::new([0_u8; 32]);
    file.read_exact(seed.as_mut())
        .map_err(|_| GatewayIdentityError::CorruptState)?;
    let mut extra = Zeroizing::new([0_u8; 1]);
    if file
        .read(extra.as_mut())
        .map_err(|_| GatewayIdentityError::Unavailable)?
        != 0
    {
        return Err(GatewayIdentityError::CorruptState);
    }
    Ok(seed)
}

fn validate_seed_length(file: &File) -> Result<(), GatewayIdentityError> {
    let metadata = file
        .metadata()
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    if metadata.len() == 32 {
        Ok(())
    } else {
        Err(GatewayIdentityError::CorruptState)
    }
}

fn read_state(path: &Path) -> Result<Vec<u8>, GatewayIdentityError> {
    let mut file =
        open_existing_private_file(path, false)?.ok_or(GatewayIdentityError::CorruptState)?;
    let metadata = file
        .metadata()
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    if metadata.len() == 0 || metadata.len() > MAX_STATE_BYTES {
        return Err(GatewayIdentityError::CorruptState);
    }
    let capacity =
        usize::try_from(metadata.len()).map_err(|_| GatewayIdentityError::CorruptState)?;
    let mut state = Vec::with_capacity(capacity);
    file.read_to_end(&mut state)
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    if state.len() != capacity {
        return Err(GatewayIdentityError::CorruptState);
    }
    Ok(state)
}

fn validate_existing_private_file(path: &Path) -> Result<(), GatewayIdentityError> {
    open_existing_private_file(path, false)?
        .map(drop)
        .ok_or(GatewayIdentityError::CorruptState)
}

fn open_existing_private_file(
    path: &Path,
    writable: bool,
) -> Result<Option<File>, GatewayIdentityError> {
    let path_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(GatewayIdentityError::Unavailable),
    };
    validate_private_file_metadata(&path_metadata)?;

    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    set_no_follow(&mut options);
    let file = options
        .open(path)
        .map_err(|_| GatewayIdentityError::InsecureStorage)?;
    validate_open_file_and_metadata(&file, &path_metadata)?;
    Ok(Some(file))
}

fn create_new_private_file(path: &Path, writable: bool) -> Result<File, GatewayIdentityError> {
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(writable);
    set_owner_only(&mut options);
    set_no_follow(&mut options);
    let file = options.open(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            GatewayIdentityError::Conflict
        } else {
            GatewayIdentityError::Unavailable
        }
    })?;
    set_exact_file_mode(&file)?;
    let metadata = file
        .metadata()
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    validate_private_file_metadata(&metadata)?;
    Ok(file)
}

fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), GatewayIdentityError> {
    let mut file = create_new_private_file(path, true)?;
    file.write_all(bytes)
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    file.sync_all()
        .map_err(|_| GatewayIdentityError::Unavailable)
}

fn validate_open_file_and_path(file: &File, path: &Path) -> Result<(), GatewayIdentityError> {
    let path_metadata =
        std::fs::symlink_metadata(path).map_err(|_| GatewayIdentityError::InsecureStorage)?;
    validate_open_file_and_metadata(file, &path_metadata)
}

fn validate_open_file_and_metadata(
    file: &File,
    path_metadata: &std::fs::Metadata,
) -> Result<(), GatewayIdentityError> {
    let file_metadata = file
        .metadata()
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    validate_private_file_metadata(path_metadata)?;
    validate_private_file_metadata(&file_metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(GatewayIdentityError::InsecureStorage);
        }
    }
    Ok(())
}

fn validate_private_file_metadata(
    metadata: &std::fs::Metadata,
) -> Result<(), GatewayIdentityError> {
    if !metadata.file_type().is_file() {
        return Err(GatewayIdentityError::InsecureStorage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o7777 != FILE_MODE
            || metadata.nlink() != 1
        {
            return Err(GatewayIdentityError::InsecureStorage);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(GatewayIdentityError::InsecureStorage)
    }
}

fn validate_state_size(state: &[u8]) -> Result<(), GatewayIdentityError> {
    if state.is_empty() || state.len() as u64 > MAX_STATE_BYTES {
        Err(GatewayIdentityError::InvalidState)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.mode(FILE_MODE);
}

#[cfg(not(unix))]
fn set_owner_only(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
}

#[cfg(not(unix))]
fn set_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_exact_file_mode(file: &File) -> Result<(), GatewayIdentityError> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(std::fs::Permissions::from_mode(FILE_MODE))
        .map_err(|_| GatewayIdentityError::Unavailable)
}

#[cfg(not(unix))]
fn set_exact_file_mode(_file: &File) -> Result<(), GatewayIdentityError> {
    Err(GatewayIdentityError::InsecureStorage)
}

fn sync_directory(path: &Path) -> Result<(), GatewayIdentityError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC);
    }
    let directory = options
        .open(path)
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    let metadata = directory
        .metadata()
        .map_err(|_| GatewayIdentityError::Unavailable)?;
    validate_directory_metadata(&metadata, false)?;
    directory
        .sync_all()
        .map_err(|_| GatewayIdentityError::Unavailable)
}

fn cleanup_staging_directory(staging: &Path, seed: &Path, state: &Path) {
    let _ = std::fs::remove_file(seed);
    let _ = std::fs::remove_file(state);
    let _ = std::fs::remove_dir(staging);
}
