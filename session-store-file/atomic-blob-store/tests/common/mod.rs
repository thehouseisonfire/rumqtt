use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static ARTIFACT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct TestDirectory {
    inner: tempfile::TempDir,
}

impl TestDirectory {
    pub(crate) fn path(&self) -> &Path {
        self.inner.path()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        let Some(artifact_root) = std::env::var_os("ATOMIC_BLOB_TEST_ARTIFACT_DIR") else {
            return;
        };
        let test_name = std::thread::current()
            .name()
            .unwrap_or("unnamed-test")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let sequence = ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let destination = PathBuf::from(artifact_root)
            .join("failed-tests")
            .join(format!("{test_name}-{}-{sequence}", std::process::id()));
        let _ = copy_directory(self.path(), &destination);
    }
}

pub(crate) fn test_directory() -> TestDirectory {
    TestDirectory {
        inner: tempfile::tempdir().expect("create test directory"),
    }
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
