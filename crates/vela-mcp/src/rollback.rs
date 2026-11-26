//! Copias de seguridad automáticas y restauración en un clic.
//!
//! El almacén es privado (0700), los blobs y el índice se escriben de forma
//! atómica con 0600, y cada alta ejecuta una poda por antigüedad y por cuota.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config::RollbackConfig, executor::ExecutorError};

pub const ROLLBACK_DIR_NAME: &str = "mcp-rollback";

const INDEX_NAME: &str = "index.json";
const BLOBS_DIR: &str = "blobs";
static SNAPSHOT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Una copia de seguridad exchangeable con el agente y con la interfaz.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupEntry {
    pub id: String,
    pub server_id: String,
    pub remote_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub mode: Option<u32>,
    pub created_at_secs: u64,
}

/// Ocupación del almacén después de las podas.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RollbackUsage {
    pub count: usize,
    pub total_bytes: u64,
    pub max_total_bytes: u64,
}

/// Almacén de copias de seguridad previas a una escritura remota.
pub struct RollbackStore {
    dir: PathBuf,
    config: RollbackConfig,
}

impl RollbackStore {
    pub fn new(dir: PathBuf, config: RollbackConfig) -> io::Result<Self> {
        let blobs = dir.join(BLOBS_DIR);
        private_dir(&dir)?;
        private_dir(&blobs)?;
        let store = Self { dir, config };
        let _guard = store.lock()?;
        if !store.index_path().exists() {
            store.write_index(&[])?;
        }
        Ok(store)
    }

    pub fn config(&self) -> &RollbackConfig {
        &self.config
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // The GUI and multiple MCP processes can use the same directory.
    fn lock(&self) -> io::Result<File> {
        let directory = File::open(&self.dir)?;
        directory.lock()?;
        Ok(directory)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_NAME)
    }

    fn blob_path(&self, id: &str) -> PathBuf {
        self.dir.join(BLOBS_DIR).join(format!("{id}.bin"))
    }

    fn read_index(&self) -> io::Result<Vec<BackupEntry>> {
        let path = self.index_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        serde_json::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
    }

    fn write_index(&self, entries: &[BackupEntry]) -> io::Result<()> {
        let text = serde_json::to_vec_pretty(entries).map_err(io::Error::other)?;
        atomic_write(&self.index_path(), &text)
    }

    /// Guarda un snapshot si está habilitado y cabe en el límite por archivo.
    pub fn record_snapshot(
        &self,
        server_id: impl Into<String>,
        remote_path: impl Into<String>,
        bytes: Vec<u8>,
        mode: Option<u32>,
        now: SystemTime,
    ) -> io::Result<Option<BackupEntry>> {
        // No se escribe un blob que `prune()` borraría de inmediato: eso
        // dejaría un `BackupEntry` colgante en la respuesta al agente.
        if !self.config.enabled
            || self.config.retention_hours == 0
            || bytes.len() as u64 > self.config.max_file_bytes
            || bytes.len() as u64 > self.config.max_total_bytes
        {
            return Ok(None);
        }
        let digest = hex(&Sha256::digest(&bytes));
        let created_at_secs = secs(now);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!(
            "{created_at_secs:016x}-{nonce:024x}-{:x}-{:x}-{}",
            std::process::id(),
            SNAPSHOT_COUNTER.fetch_add(1, Ordering::Relaxed),
            &digest[..16]
        );
        let entry = BackupEntry {
            id: id.clone(),
            server_id: server_id.into(),
            remote_path: remote_path.into(),
            size_bytes: bytes.len() as u64,
            sha256: digest,
            mode,
            created_at_secs,
        };
        {
            let _guard = self.lock()?;
            atomic_write(&self.blob_path(&id), &bytes)?;
            let mut entries = self.read_index()?;
            entries.push(entry.clone());
            self.write_index(&entries)?;
        }
        self.prune(now)?;
        Ok(Some(entry))
    }

    fn expired(entry: &BackupEntry, now: SystemTime, retention_hours: u64) -> bool {
        let lifetime = retention_hours.saturating_mul(3600);
        now.duration_since(UNIX_EPOCH + std::time::Duration::from_secs(entry.created_at_secs))
            .is_ok_and(|age| age.as_secs() >= lifetime)
    }

    /// Elimina caducados y evicta los más antiguos hasta cumplir la cuota total.
    pub fn prune(&self, now: SystemTime) -> io::Result<usize> {
        let _guard = self.lock()?;
        let mut entries = self.read_index()?;
        let before = entries.len();
        let mut removed: Vec<BackupEntry> = Vec::new();
        entries.retain(|entry| {
            if Self::expired(entry, now, self.config.retention_hours) {
                removed.push(entry.clone());
                false
            } else {
                true
            }
        });

        entries.sort_by(|a, b| {
            a.created_at_secs
                .cmp(&b.created_at_secs)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut total: u64 = entries.iter().map(|entry| entry.size_bytes).sum();
        while total > self.config.max_total_bytes
            && let Some(entry) = entries.first().cloned()
        {
            total = total.saturating_sub(entry.size_bytes);
            removed.push(entry);
            entries.remove(0);
        }

        let changed = !removed.is_empty() || entries.len() != before;
        if changed {
            for entry in &removed {
                let _ = fs::remove_file(self.blob_path(&entry.id));
            }
            self.write_index(&entries)?;
        }
        Ok(removed.len())
    }

    pub fn get_snapshot(
        &self,
        id: &str,
        now: SystemTime,
    ) -> io::Result<Option<(BackupEntry, Vec<u8>)>> {
        let _guard = self.lock()?;
        let Some(entry) = self.read_index()?.into_iter().find(|entry| entry.id == id) else {
            return Ok(None);
        };
        if Self::expired(&entry, now, self.config.retention_hours) {
            return Ok(None);
        }
        let bytes = fs::read(self.blob_path(id))?;
        Ok(Some((entry, bytes)))
    }

    pub fn list(&self, now: SystemTime) -> io::Result<Vec<BackupEntry>> {
        let _guard = self.lock()?;
        let mut entries: Vec<_> = self
            .read_index()?
            .into_iter()
            .filter(|entry| !Self::expired(entry, now, self.config.retention_hours))
            .collect();
        entries.sort_by(|a, b| {
            a.created_at_secs
                .cmp(&b.created_at_secs)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(entries)
    }

    pub fn usage(&self, now: SystemTime) -> io::Result<RollbackUsage> {
        let entries = self.list(now)?;
        Ok(RollbackUsage {
            count: entries.len(),
            total_bytes: entries.iter().map(|entry| entry.size_bytes).sum(),
            max_total_bytes: self.config.max_total_bytes,
        })
    }

    /// Vacia el almacén y devuelve el número de copias eliminadas.
    pub fn clear(&self) -> io::Result<usize> {
        let _guard = self.lock()?;
        let entries = self.read_index()?;
        for entry in &entries {
            match fs::remove_file(self.blob_path(&entry.id)) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        match fs::remove_file(self.index_path()) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        self.write_index(&[])?;
        Ok(entries.len())
    }
}

/// Reaplica los bytes y el modo exactos de una copia en la ruta remota original.
pub async fn restore_snapshot(
    session: &vela_core::Session,
    store: &RollbackStore,
    id: &str,
    now: SystemTime,
) -> Result<BackupEntry, ExecutorError> {
    let (entry, bytes) = store
        .get_snapshot(id, now)?
        .ok_or_else(|| ExecutorError::SnapshotNotFound(id.to_owned()))?;
    session
        .write_bytes(PathBuf::from(&entry.remote_path), bytes)
        .await?;
    if let Some(mode) = entry.mode {
        session
            .chmod(PathBuf::from(&entry.remote_path), mode)
            .await?;
    }
    Ok(entry)
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(unix)]
fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store(config: RollbackConfig) -> (tempfile::TempDir, RollbackStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = RollbackStore::new(dir.path().join("rollback"), config).unwrap();
        (dir, store)
    }

    #[test]
    fn independent_stores_can_record_to_the_same_directory() {
        let (_dir, first) = store(RollbackConfig::default());
        let other = RollbackStore::new(first.dir().to_owned(), RollbackConfig::default()).unwrap();
        let together = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for store in [&first, &other] {
                let together = &together;
                scope.spawn(move || {
                    together.wait();
                    for _ in 0..8 {
                        store
                            .record_snapshot("srv", "/same", b"identical".to_vec(), None, at(100))
                            .unwrap()
                            .unwrap();
                    }
                });
            }
        });
        let copies = first.list(at(100)).unwrap();
        assert_eq!(copies.len(), 16);
        assert!(
            copies
                .iter()
                .all(|copy| first.get_snapshot(&copy.id, at(100)).unwrap().is_some())
        );
    }

    #[test]
    fn snapshot_is_private_indexed_and_readable() {
        let (_dir, store) = store(RollbackConfig::default());
        let entry = store
            .record_snapshot(
                "srv",
                "/srv/api/app.js",
                b"hola".to_vec(),
                Some(0o640),
                at(100),
            )
            .unwrap()
            .unwrap();
        assert_eq!(entry.size_bytes, 4);
        assert_eq!(entry.sha256, hex(&Sha256::digest(b"hola")));
        let (loaded, bytes) = store.get_snapshot(&entry.id, at(100)).unwrap().unwrap();
        assert_eq!(loaded, entry);
        assert_eq!(bytes, b"hola");
        assert_eq!(store.list(at(100)).unwrap().len(), 1);
        assert_eq!(store.usage(at(100)).unwrap().total_bytes, 4);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(store.dir()), 0o700);
            assert_eq!(mode(&store.dir().join(BLOBS_DIR)), 0o700);
            assert_eq!(mode(&store.index_path()), 0o600);
            assert_eq!(mode(&store.blob_path(&entry.id)), 0o600);
        }
    }

    #[test]
    fn disabled_and_oversized_files_are_not_snapshotted() {
        let (_dir, limited) = store(RollbackConfig {
            max_file_bytes: 4,
            ..RollbackConfig::default()
        });
        assert!(
            limited
                .record_snapshot("srv", "/grande", b"12345".to_vec(), None, at(100))
                .unwrap()
                .is_none()
        );
        let (_dir, disabled) = store(RollbackConfig {
            enabled: false,
            ..RollbackConfig::default()
        });
        assert!(
            disabled
                .record_snapshot("srv", "/x", b"x".to_vec(), None, at(100))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn snapshots_larger_than_the_total_quota_and_zero_retention_are_skipped() {
        let (_dir, quota_store) = store(RollbackConfig {
            max_total_bytes: 4,
            ..RollbackConfig::default()
        });
        assert!(
            quota_store
                .record_snapshot("srv", "/grande", b"12345".to_vec(), None, at(100))
                .unwrap()
                .is_none()
        );
        assert!(quota_store.list(at(100)).unwrap().is_empty());
        assert_eq!(
            fs::read_dir(quota_store.dir().join(BLOBS_DIR))
                .unwrap()
                .count(),
            0
        );

        let (_dir, no_retention) = store(RollbackConfig {
            retention_hours: 0,
            ..RollbackConfig::default()
        });
        assert!(
            no_retention
                .record_snapshot("srv", "/x", b"x".to_vec(), None, at(100))
                .unwrap()
                .is_none()
        );
        assert!(no_retention.list(at(100)).unwrap().is_empty());
    }

    #[test]
    fn expired_snapshots_are_pruned_after_twenty_four_hours() {
        let (_dir, store) = store(RollbackConfig::default());
        let old = store
            .record_snapshot("srv", "/viejo", b"a".to_vec(), None, at(100))
            .unwrap()
            .unwrap();
        let recent = store
            .record_snapshot("srv", "/reciente", b"b".to_vec(), None, at(100 + 25 * 3600))
            .unwrap()
            .unwrap();
        assert_eq!(store.list(at(100 + 25 * 3600)).unwrap().len(), 1);
        assert!(
            store
                .get_snapshot(&old.id, at(100 + 25 * 3600))
                .unwrap()
                .is_none()
        );
        assert!(!store.blob_path(&old.id).exists());
        assert!(
            store
                .get_snapshot(&recent.id, at(100 + 25 * 3600))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn total_quota_evicts_the_oldest_snapshots() {
        let (_dir, store) = store(RollbackConfig {
            max_total_bytes: 10,
            ..RollbackConfig::default()
        });
        let mut ids = Vec::new();
        for index in 0..4 {
            let entry = store
                .record_snapshot(
                    "srv",
                    format!("/f{index}"),
                    b"abcd".to_vec(),
                    None,
                    at(100 + index),
                )
                .unwrap()
                .unwrap();
            ids.push(entry.id);
        }
        let usage = store.usage(at(200)).unwrap();
        assert!(usage.total_bytes <= 10);
        assert_eq!(usage.count, 2);
        assert!(store.get_snapshot(&ids[0], at(200)).unwrap().is_none());
        assert!(store.get_snapshot(&ids[1], at(200)).unwrap().is_none());
        assert!(store.get_snapshot(&ids[3], at(200)).unwrap().is_some());
    }

    #[test]
    fn clear_removes_every_blob_and_resets_the_index() {
        let (_dir, store) = store(RollbackConfig::default());
        for index in 0..3 {
            store
                .record_snapshot(
                    "srv",
                    format!("/x{index}"),
                    vec![index; 3],
                    None,
                    at(100 + index as u64),
                )
                .unwrap();
        }
        assert_eq!(store.clear().unwrap(), 3);
        assert!(store.list(at(200)).unwrap().is_empty());
        assert_eq!(
            fs::read_dir(store.dir().join(BLOBS_DIR)).unwrap().count(),
            0
        );
        assert_eq!(store.clear().unwrap(), 0);
    }
}
