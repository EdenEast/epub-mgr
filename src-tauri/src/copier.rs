use std::{
    ffi::OsString,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    OutputExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyError {
    message: String,
}

impl CopyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(action: &str, path: &Path, error: io::Error) -> Self {
        Self::new(format!("failed to {action} {}: {error}", path.display()))
    }
}

impl fmt::Display for CopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CopyError {}

pub fn copy_cleaned_epub(source_path: &Path, target_path: &Path) -> Result<CopyOutcome, CopyError> {
    if target_exists(target_path)? {
        return Ok(CopyOutcome::OutputExists);
    }

    let parent = target_path.parent().ok_or_else(|| {
        CopyError::new(format!(
            "Cleaned EPUB output path has no parent directory: {}",
            target_path.display()
        ))
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| CopyError::io("create parent directories for", target_path, error))?;

    if target_exists(target_path)? {
        return Ok(CopyOutcome::OutputExists);
    }

    let (temp_path, temp_file) = create_unique_temp_file(parent, target_path)?;

    match copy_via_temp(source_path, target_path, &temp_path, temp_file) {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(with_temp_cleanup(error, &temp_path)),
    }
}

fn copy_via_temp(
    source_path: &Path,
    target_path: &Path,
    temp_path: &Path,
    mut temp_file: File,
) -> Result<CopyOutcome, CopyError> {
    let mut source_file = File::open(source_path)
        .map_err(|error| CopyError::io("open source EPUB", source_path, error))?;
    io::copy(&mut source_file, &mut temp_file).map_err(|error| {
        CopyError::io("copy source EPUB bytes to temporary file", temp_path, error)
    })?;
    temp_file
        .flush()
        .map_err(|error| CopyError::io("flush temporary file", temp_path, error))?;
    temp_file
        .sync_all()
        .map_err(|error| CopyError::io("sync temporary file", temp_path, error))?;
    drop(temp_file);

    match rename_without_replace(temp_path, target_path) {
        Ok(()) => {
            sync_parent_directory(target_path);
            Ok(CopyOutcome::Copied)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            remove_temp_file(temp_path).map_err(|cleanup_error| {
                CopyError::new(format!(
                    "output path already exists: {}; additionally failed to remove temporary file {}: {cleanup_error}",
                    target_path.display(),
                    temp_path.display()
                ))
            })?;
            Ok(CopyOutcome::OutputExists)
        }
        Err(error) => Err(CopyError::io(
            "place temporary file at final Cleaned EPUB path",
            target_path,
            error,
        )),
    }
}

fn target_exists(target_path: &Path) -> Result<bool, CopyError> {
    target_path
        .try_exists()
        .map_err(|error| CopyError::io("check whether output exists", target_path, error))
}

fn create_unique_temp_file(
    parent: &Path,
    target_path: &Path,
) -> Result<(PathBuf, File), CopyError> {
    for _ in 0..100 {
        let temp_path = parent.join(unique_temp_file_name(target_path)?);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CopyError::io(
                    "create unique temporary file in target directory",
                    &temp_path,
                    error,
                ))
            }
        }
    }

    Err(CopyError::new(format!(
        "failed to create a unique temporary file in {} after 100 attempts",
        parent.display()
    )))
}

fn unique_temp_file_name(target_path: &Path) -> Result<OsString, CopyError> {
    let final_name = target_path.file_name().ok_or_else(|| {
        CopyError::new(format!(
            "Cleaned EPUB output path has no file name: {}",
            target_path.display()
        ))
    })?;
    let counter = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = OsString::from(".");
    temp_name.push(final_name);
    temp_name.push(format!(".tmp.{}.{}", std::process::id(), counter));
    Ok(temp_name)
}

fn with_temp_cleanup(error: CopyError, temp_path: &Path) -> CopyError {
    match remove_temp_file(temp_path) {
        Ok(()) => error,
        Err(cleanup_error) if cleanup_error.kind() == io::ErrorKind::NotFound => error,
        Err(cleanup_error) => CopyError::new(format!(
            "{error}; additionally failed to remove temporary file {}: {cleanup_error}",
            temp_path.display()
        )),
    }
}

fn remove_temp_file(temp_path: &Path) -> io::Result<()> {
    fs::remove_file(temp_path)
}

fn sync_parent_directory(target_path: &Path) {
    let Some(parent) = target_path.parent() else {
        return;
    };
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox"
))]
fn rename_without_replace(source_path: &Path, target_path: &Path) -> io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source_path,
        rustix::fs::CWD,
        target_path,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

#[cfg(windows)]
fn rename_without_replace(source_path: &Path, target_path: &Path) -> io::Result<()> {
    // Windows file rename fails when the target path already exists, preserving
    // the no-overwrite invariant without a separate existence check.
    fs::rename(source_path, target_path)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "redox",
    windows
)))]
fn rename_without_replace(_source_path: &Path, _target_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-overwrite rename is not implemented on this platform",
    ))
}
