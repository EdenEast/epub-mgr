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

    let (temp_path, temp_file) =
        create_unique_temp_file_with_next_temp_id(parent, target_path, &NEXT_TEMP_ID)?;

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

fn create_unique_temp_file_with_next_temp_id(
    parent: &Path,
    target_path: &Path,
    next_temp_id: &AtomicU64,
) -> Result<(PathBuf, File), CopyError> {
    for _ in 0..100 {
        let temp_path = parent.join(unique_temp_file_name(target_path, next_temp_id)?);
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

fn unique_temp_file_name(
    target_path: &Path,
    next_temp_id: &AtomicU64,
) -> Result<OsString, CopyError> {
    let counter = next_temp_id.fetch_add(1, Ordering::Relaxed);
    unique_temp_file_name_for_counter(target_path, counter)
}

fn unique_temp_file_name_for_counter(
    target_path: &Path,
    counter: u64,
) -> Result<OsString, CopyError> {
    let final_name = target_path.file_name().ok_or_else(|| {
        CopyError::new(format!(
            "Cleaned EPUB output path has no file name: {}",
            target_path.display()
        ))
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn create_unique_temp_file_uses_a_fresh_name_when_a_candidate_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target_path = temp.path().join("output").join("nested").join("book.epub");
        let target_parent = target_path.parent().expect("target parent");
        let next_temp_id = AtomicU64::new(0);
        fs::create_dir_all(target_parent).expect("create target parent");

        let reserved_temp_path = target_parent.join(
            unique_temp_file_name_for_counter(&target_path, 0)
                .expect("reserved temporary file name"),
        );
        fs::write(&reserved_temp_path, b"reserved temp must survive")
            .expect("reserve next temp candidate");

        let (temp_path, _temp_file) =
            create_unique_temp_file_with_next_temp_id(target_parent, &target_path, &next_temp_id)
                .expect("create unique temp file");

        assert_ne!(temp_path, reserved_temp_path);
        assert_eq!(temp_path.parent(), Some(target_parent));
        assert!(temp_path.exists());
        assert_eq!(
            fs::read(&reserved_temp_path).expect("read reserved temp"),
            b"reserved temp must survive"
        );
    }
}
