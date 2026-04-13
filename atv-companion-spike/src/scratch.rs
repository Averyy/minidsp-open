use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone)]
pub struct ScratchPaths {
    pub root: PathBuf,
    pub devices: PathBuf,
    pub credentials: PathBuf,
    pub metadata: PathBuf,
    pub matches: PathBuf,
    pub debug: PathBuf,
}

impl ScratchPaths {
    pub fn new(root: PathBuf) -> Self {
        Self {
            devices: root.join("devices.json"),
            credentials: root.join("credentials.json"),
            metadata: root.join("metadata.jsonl"),
            matches: root.join("matches.jsonl"),
            debug: root.join("debug"),
            root,
        }
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("failed to create {}", self.root.display()))?;
        fs::create_dir_all(&self.debug)
            .with_context(|| format!("failed to create {}", self.debug.display()))?;
        set_restrictive_mode(&self.root)?;
        set_restrictive_mode(&self.debug)?;

        self.ensure_file(&self.devices, b"{\"devices\":{}}\n", false)?;
        self.ensure_file(&self.credentials, b"{\"by_device\":{}}\n", true)?;
        self.ensure_file(&self.metadata, b"", false)?;
        self.ensure_file(&self.matches, b"", false)?;
        Ok(())
    }

    pub fn read_json<T>(&self, path: &Path) -> Result<T>
    where
        T: DeserializeOwned + Default,
    {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.is_empty() {
            return Ok(T::default());
        }
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to decode {}", path.display()))
    }

    pub fn write_json<T>(&self, path: &Path, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        self.write_json_inner(path, value, false)
    }

    pub fn write_json_secure<T>(&self, path: &Path, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        self.write_json_inner(path, value, true)
    }

    pub fn append_jsonl<T>(&self, path: &Path, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        let mut encoded = serde_json::to_vec(value).context("failed to encode JSONL entry")?;
        encoded.push(b'\n');
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.write_all(&encoded)
            .with_context(|| format!("failed to append {}", path.display()))?;
        Ok(())
    }

    fn ensure_file(&self, path: &Path, default: &[u8], secure: bool) -> Result<()> {
        if !path.exists() {
            fs::write(path, default)
                .with_context(|| format!("failed to create {}", path.display()))?;
        }
        if secure {
            set_secret_mode(path)?;
        }
        Ok(())
    }

    fn write_json_inner<T>(&self, path: &Path, value: &T, secure: bool) -> Result<()>
    where
        T: Serialize,
    {
        let encoded = serde_json::to_vec_pretty(value)
            .with_context(|| format!("failed to encode {}", path.display()))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, encoded).with_context(|| format!("failed to write {}", tmp.display()))?;
        if secure {
            set_secret_mode(&tmp)?;
        }
        fs::rename(&tmp, path).with_context(|| format!("failed to replace {}", path.display()))?;
        if secure {
            set_secret_mode(path)?;
        }
        Ok(())
    }
}

fn set_restrictive_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn set_secret_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}
