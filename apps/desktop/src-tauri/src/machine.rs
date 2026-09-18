//! This installation's identity on the job queue.
//!
//! Every lease on the `jobs` table names the machine that holds it, and a
//! machine starting up recovers *only* the jobs it left running itself. That
//! needs an id that survives restarts but differs between installations — a
//! random one generated once and kept beside the rest of the machine's local
//! state. It is deliberately not the hostname: two laptops can share one, and a
//! reinstall on the same box should not inherit the old install's leases.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "machine-id";

/// Reads the id, creating one on first run. A file that cannot be written
/// still yields a usable id for this process; it just will not be stable.
pub fn load_or_create(local_state_dir: &Path) -> String {
    let path = id_path(local_state_dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if is_plausible(trimmed) {
            return trimmed.to_string();
        }
    }
    let fresh = uuid::Uuid::new_v4().to_string();
    if let Err(error) = std::fs::create_dir_all(local_state_dir).and_then(|_| std::fs::write(&path, &fresh)) {
        tracing::warn!(%error, path = %path.display(), "could not persist the machine id; it will change on restart");
    }
    fresh
}

fn id_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

fn is_plausible(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_created_once_and_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path());
        let second = load_or_create(dir.path());
        assert_eq!(first, second);
        assert!(is_plausible(&first));

        let other = tempfile::tempdir().unwrap();
        assert_ne!(load_or_create(other.path()), first, "installations differ");
    }

    #[test]
    fn a_corrupt_file_is_replaced_rather_than_trusted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(id_path(dir.path()), "not an id; has spaces and \n newlines").unwrap();
        let id = load_or_create(dir.path());
        assert!(is_plausible(&id));
        assert_eq!(load_or_create(dir.path()), id);
    }
}
