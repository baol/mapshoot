use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

/// Default cache lifetime: one week.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Default cache size cap: 512 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

const LAYOUT_VERSION: &str = "v1";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A persistent, per-user tile cache.
///
/// Tile URLs are hashed before they reach the filesystem, so provider keys and
/// query strings are never stored in cache paths. Entries older than the TTL
/// are ignored and removed; the cache is trimmed to its size cap from oldest to
/// newest. Cache failures never fail a render.
pub struct Cache {
    root: PathBuf,
    ttl: Duration,
    max_bytes: u64,
    bytes: AtomicU64,
    trimming: AtomicBool,
}

impl Cache {
    /// Open the system cache with the default lifetime and size cap.
    pub fn system() -> Option<Self> {
        Self::system_with(DEFAULT_TTL, DEFAULT_MAX_BYTES)
    }

    /// Open the system cache with custom limits.
    ///
    /// `MAPSHOOT_CACHE_DIR` overrides the platform directory; an empty value
    /// disables the cache. Failures to create or inspect the cache return
    /// `None`.
    pub fn system_with(ttl: Duration, max_bytes: u64) -> Option<Self> {
        let root = if let Some(value) = std::env::var_os("MAPSHOOT_CACHE_DIR") {
            if value.is_empty() {
                return None;
            }
            PathBuf::from(value)
        } else {
            platform_cache_root()?
        };
        Self::at(root, ttl, max_bytes).ok()
    }

    /// Open a cache below `root`, creating `<root>/v1` if necessary.
    ///
    /// Directories are created private to the current user on Unix, and cache
    /// files are written atomically with private permissions.
    pub fn at(root: impl AsRef<Path>, ttl: Duration, max_bytes: u64) -> io::Result<Self> {
        if root.as_ref().as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache directory must not be empty",
            ));
        }
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cache size limit must be positive",
            ));
        }
        let root = root.as_ref().join(LAYOUT_VERSION);
        create_private_dir(&root)?;
        let cache = Self {
            root,
            ttl,
            max_bytes,
            bytes: AtomicU64::new(0),
            trimming: AtomicBool::new(false),
        };
        if let Ok(total) = cache.sweep() {
            cache.bytes.store(total, Ordering::Relaxed);
        }
        Ok(cache)
    }

    /// The versioned cache directory actually in use.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn get(&self, url: &str, tile_size: u32) -> Option<Vec<u8>> {
        let path = self.path(url, tile_size);
        let metadata = fs::metadata(&path).ok()?;
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if self.expired(modified) {
            let _ = fs::remove_file(&path);
            return None;
        }
        fs::read(&path).ok()
    }

    pub(crate) fn put(&self, url: &str, tile_size: u32, data: &[u8]) {
        let path = self.path(url, tile_size);
        let Some(parent) = path.parent() else {
            return;
        };
        if create_private_dir(parent).is_err() {
            return;
        }
        if let Ok(metadata) = fs::metadata(&path) {
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if !self.expired(modified) {
                return;
            }
            let _ = fs::remove_file(&path);
        }
        let Some(stem) = path.file_stem() else {
            return;
        };
        let temp = parent.join(format!(
            ".{}.{}.{}.tmp",
            stem.to_string_lossy(),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        if write_private_file(&temp, data).is_ok() && fs::rename(&temp, &path).is_ok() {
            let total =
                self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed) + data.len() as u64;
            if total > self.max_bytes {
                self.trim_if_needed();
            }
        }
        let _ = fs::remove_file(temp);
    }

    pub(crate) fn remove(&self, url: &str, tile_size: u32) {
        let _ = fs::remove_file(self.path(url, tile_size));
    }

    fn path(&self, url: &str, tile_size: u32) -> PathBuf {
        let key = cache_key(url, tile_size);
        self.root
            .join("tiles")
            .join(&key[..2])
            .join(format!("{key}.png"))
    }

    fn expired(&self, modified: SystemTime) -> bool {
        SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > self.ttl)
    }

    fn trim_if_needed(&self) {
        if self.bytes.load(Ordering::Relaxed) <= self.max_bytes {
            return;
        }
        if self
            .trimming
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        if let Ok(total) = self.sweep() {
            self.bytes.store(total, Ordering::Relaxed);
        }
        self.trimming.store(false, Ordering::Release);
    }

    /// Remove expired entries and evict the oldest files until the cache is at
    /// most 90% of its size cap. Returns the remaining size in bytes.
    fn sweep(&self) -> io::Result<u64> {
        struct Entry {
            path: PathBuf,
            modified: SystemTime,
            len: u64,
        }

        let tiles = self.root.join("tiles");
        let shards = match fs::read_dir(&tiles) {
            Ok(shards) => shards,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(err),
        };
        let now = SystemTime::now();
        let mut total = 0_u64;
        let mut entries = Vec::new();
        for shard in shards.flatten() {
            if !shard.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(files) = fs::read_dir(shard.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
                    continue;
                }
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let expired = now.duration_since(modified).is_ok_and(|age| age > self.ttl);
                if expired {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                total += metadata.len();
                entries.push(Entry {
                    path,
                    modified,
                    len: metadata.len(),
                });
            }
        }
        if total > self.max_bytes {
            entries.sort_by(|a, b| {
                a.modified
                    .cmp(&b.modified)
                    .then_with(|| a.path.cmp(&b.path))
            });
            let target = self.max_bytes - self.max_bytes / 10;
            for entry in entries {
                if total <= target {
                    break;
                }
                if fs::remove_file(&entry.path).is_ok() {
                    total = total.saturating_sub(entry.len);
                }
            }
        }
        Ok(total)
    }
}

fn cache_key(url: &str, tile_size: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"mapshoot-cache\0");
    hasher.update(LAYOUT_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(tile_size.to_le_bytes());
    hasher.update(url.as_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

fn write_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)
}

#[cfg(target_os = "macos")]
fn platform_cache_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join("Library/Caches/mapshoot"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_root() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(xdg);
        if path.is_absolute() {
            return Some(path.join("mapshoot"));
        }
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".cache/mapshoot"))
}

#[cfg(target_os = "windows")]
fn platform_cache_root() -> Option<PathBuf> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(local).join("mapshoot"));
    }
    std::env::var_os("USERPROFILE")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join("AppData/Local/mapshoot"))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn platform_cache_root() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("mapshoot-cache-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn files(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("png") {
                    found.push(path);
                }
            }
        }
        found
    }

    #[test]
    fn roundtrip_hashes_urls_and_separates_tile_sizes() {
        let dir = temporary("roundtrip");
        let cache = Cache::at(&dir, Duration::from_secs(3600), 1024 * 1024).unwrap();
        assert!(cache.get("https://example.com/1?key=SECRET", 256).is_none());
        cache.put("https://example.com/1?key=SECRET", 256, b"pixels");
        assert_eq!(
            cache.get("https://example.com/1?key=SECRET", 256),
            Some(b"pixels".to_vec())
        );
        assert!(cache.get("https://example.com/1?key=SECRET", 512).is_none());
        let path = cache.path("https://example.com/1?key=SECRET", 256);
        assert!(!path.to_string_lossy().contains("SECRET"));
        assert!(!path.to_string_lossy().contains("example.com"));
        assert_eq!(path.extension().unwrap(), "png");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
            let dir_mode = fs::metadata(cache.root()).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o077, 0, "{dir_mode:o}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn expired_entries_are_removed_and_missing_again() {
        let dir = temporary("expiry");
        let cache = Cache::at(&dir, Duration::from_secs(3600), 1024 * 1024).unwrap();
        cache.put("https://example.com/tile", 256, b"stale");
        let path = cache.path("https://example.com/tile", 256);
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        assert!(cache.get("https://example.com/tile", 256).is_none());
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn opening_a_cache_removes_expired_entries() {
        let dir = temporary("sweep");
        let cache = Cache::at(&dir, Duration::from_secs(3600), 1024 * 1024).unwrap();
        cache.put("https://example.com/tile", 256, b"stale");
        let path = cache.path("https://example.com/tile", 256);
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        let cache = Cache::at(&dir, Duration::from_secs(3600), 1024 * 1024).unwrap();
        assert!(!path.exists());
        assert_eq!(files(cache.root()).len(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_cache_evicts_oldest_entries() {
        let dir = temporary("evict");
        let cache = Cache::at(&dir, Duration::from_secs(3600), 250).unwrap();
        for index in 0..5 {
            cache.put(&format!("https://example.com/{index}"), 256, &[0; 100]);
        }
        let count = files(cache.root()).len();
        assert!(count <= 2, "{count}");
        assert!(count > 0, "cache evicted everything");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_directory_and_zero_cap_are_rejected() {
        assert!(Cache::at("", DEFAULT_TTL, DEFAULT_MAX_BYTES).is_err());
        let dir = temporary("zero");
        assert!(Cache::at(&dir, DEFAULT_TTL, 0).is_err());
        assert!(!dir.exists());
    }
}
