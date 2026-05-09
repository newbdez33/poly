use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// Disk cache rooted at a directory, storing JSON files keyed by filename.
pub struct DiskCache {
    root: PathBuf,
}

impl DiskCache {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating cache dir {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.json"))
    }

    pub fn exists(&self, key: &str) -> bool {
        self.path_for(key).exists()
    }

    pub fn read<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let path = self.path_for(key);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading cache {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding cache {}", path.display()))
    }

    pub fn write<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(value)?;
        std::fs::write(&path, bytes)
            .with_context(|| format!("writing cache {}", path.display()))?;
        Ok(())
    }

    /// Default cache root: ~/.poly-backtest-cache/<subdir>
    pub fn default_root(subdir: &str) -> PathBuf {
        match dirs::home_dir() {
            Some(home) => home.join(".poly-backtest-cache").join(subdir),
            None => PathBuf::from("./poly-backtest-cache").join(subdir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        n: i32,
        s: String,
    }

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        let v = Sample { n: 42, s: "hello".into() };
        cache.write("foo", &v).unwrap();
        let back: Sample = cache.read("foo").unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn exists_reflects_writes() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        assert!(!cache.exists("k"));
        cache.write("k", &Sample { n: 1, s: "x".into() }).unwrap();
        assert!(cache.exists("k"));
    }

    #[test]
    fn read_missing_returns_error() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        let r: Result<Sample> = cache.read("nonexistent");
        assert!(r.is_err());
    }
}
