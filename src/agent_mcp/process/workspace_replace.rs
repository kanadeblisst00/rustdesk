use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) struct Replacement {
    root: PathBuf,
    backup: String,
    expected: String,
}

impl Replacement {
    pub(super) fn new(root: &Path, path: &str, options: &Value) -> Result<Self, String> {
        let expected = options["expected_sha256"]
            .as_str()
            .ok_or("Missing replace.expected_sha256")?;
        if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("replace.expected_sha256 must contain 64 hexadecimal characters".into());
        }
        let expected = expected.to_ascii_lowercase();
        Ok(Self {
            root: root.to_owned(),
            backup: format!(
                "reports/.mcp-backups/{:x}/{expected}.bak",
                Sha256::digest(path.as_bytes())
            ),
            expected,
        })
    }

    pub(super) fn info(&self) -> Value {
        json!({"expected_sha256":self.expected,"backup_path":self.backup,
            "backup_remote_path":self.root.join(&self.backup),"backup_sha256":self.expected})
    }

    pub(super) fn check_hash(&self, hash: &str) -> Result<(), String> {
        if hash != self.expected {
            return Err("Replacement checksum conflict: current file differs from replace.expected_sha256; no overwrite performed".into());
        }
        Ok(())
    }

    fn check_current(&self, path: &Path) -> Result<(), String> {
        let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("Replacement target must be a regular file without links".into());
        }
        self.check_hash(&super::digest(path)?.1)
    }

    fn verify_backup(&self) -> Result<(), String> {
        let backup = super::target(&self.root, &self.backup, false)?;
        if super::digest(&backup)?.1 != self.expected {
            return Err("Replacement backup checksum conflict; no overwrite performed".into());
        }
        Ok(())
    }

    fn backup(&self, original: &Path) -> Result<(), String> {
        let backup = super::target(&self.root, &self.backup, true)?;
        if backup.exists() {
            return self.verify_backup();
        }
        let temporary = backup.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut copy = options.open(&temporary).map_err(|e| e.to_string())?;
        let result: Result<(), String> = (|| {
            let mut original = fs::File::open(original)
                .map_err(|e| e.to_string())?
                .take(512 * 1024 * 1024 + 1);
            std::io::copy(&mut original, &mut copy).map_err(|e| e.to_string())?;
            copy.sync_all().map_err(|e| e.to_string())?;
            drop(copy);
            if super::digest(&temporary)?.1 != self.expected {
                return Err(
                    "Original changed while making replacement backup; no overwrite performed"
                        .into(),
                );
            }
            fs::hard_link(&temporary, &backup)
                .map_err(|e| format!("Publish replacement backup: {e}"))?;
            Ok(())
        })();
        if let Err(error) = fs::remove_file(&temporary) {
            return Err(format!(
                "Remove temporary replacement backup: {error}; backup result: {result:?}"
            ));
        }
        result
    }

    pub(super) fn completed(&self, target: &Path, hash: &str) -> Result<(), String> {
        if hash == self.expected {
            self.backup(target)
        } else {
            self.verify_backup()
        }
    }

    pub(super) fn publish(&self, staged: &Path, target: &Path) -> Result<(), String> {
        self.check_current(target)?;
        self.backup(target)?;
        self.check_current(target)?;
        super::super::platform::replace(staged, target)
    }
}
