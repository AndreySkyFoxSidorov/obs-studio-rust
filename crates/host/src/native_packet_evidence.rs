//! Test-only opt-in native packet evidence. No overwrites or implicit cleanup.
use serde::Serialize;
use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
};
const MAX_PACKETS: usize = 128;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADERS: usize = 32 * 1024;
const MAX_FILES: usize = 264;
const MAX_DIRECTORIES: usize = 4;
#[derive(Default)]
struct Budget {
    packets: usize,
    bytes: usize,
    headers: usize,
    header_count: usize,
    files: usize,
    directories: usize,
}
impl Budget {
    fn bytes(&mut self, count: usize) -> io::Result<()> {
        if count > MAX_BYTES.saturating_sub(self.bytes) {
            return Err(limit("native evidence exceeds16MiB"));
        }
        self.bytes += count;
        Ok(())
    }
    fn packet(&mut self) -> io::Result<()> {
        if self.packets >= MAX_PACKETS {
            return Err(limit("native evidence exceeds128packets"));
        }
        self.packets += 1;
        Ok(())
    }
    fn header(&mut self, count: usize) -> io::Result<()> {
        if count == 0
            || count > MAX_HEADERS.saturating_sub(self.headers)
            || self.header_count >= MAX_PACKETS
        {
            return Err(limit("native evidence exceeds32KiB/128headers"));
        }
        self.headers += count;
        self.header_count += 1;
        Ok(())
    }
    fn file(&mut self) -> io::Result<()> {
        if self.files >= MAX_FILES {
            return Err(limit("native evidence file-count bound"));
        }
        self.files += 1;
        Ok(())
    }
    fn directory(&mut self) -> io::Result<()> {
        if self.directories >= MAX_DIRECTORIES {
            return Err(limit("native evidence directory-count bound"));
        }
        self.directories += 1;
        Ok(())
    }
}
fn limit(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn name(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 96
        || value.starts_with('.')
        || value.ends_with('.')
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err(limit(
            "native evidence filename is not a bounded literal component",
        ));
    }
    Ok(())
}
#[derive(Clone)]
pub(crate) struct Directory {
    path: PathBuf,
    budget: Rc<RefCell<Budget>>,
}
impl Directory {
    pub fn from_env(variable: &str) -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os(variable) else {
            return Ok(None);
        };
        Self::new(PathBuf::from(path)).map(Some)
    }
    fn new(path: PathBuf) -> io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() || path.to_str().is_none() {
            return Err(limit("evidence env must name an absolute NEW directory"));
        }
        // Do not create arbitrary parent paths or reuse a directory/symlink. The
        // caller explicitly owns the parent; AlreadyExists preserves every byte.
        fs::create_dir(&path)?;
        let directory = Self {
            path,
            budget: Rc::new(RefCell::new(Budget::default())),
        };
        directory.budget.borrow_mut().directory()?;
        Ok(directory)
    }
    pub fn child(&self, child: &str) -> io::Result<Self> {
        name(child)?;
        self.budget.borrow_mut().directory()?;
        let path = self.path.join(child);
        fs::create_dir(&path)?;
        Ok(Self {
            path,
            budget: self.budget.clone(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn counters_before_receipt(&self) -> serde_json::Value {
        let budget = self.budget.borrow();
        serde_json::json!({"packets":budget.packets,"written_bytes":budget.bytes,"header_bytes":budget.headers,"header_files":budget.header_count,"files":budget.files,"directories":budget.directories})
    }
    pub fn create(&self, filename: &str) -> io::Result<BoundedFile> {
        name(filename)?;
        self.budget.borrow_mut().file()?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(filename))?;
        Ok(BoundedFile {
            file,
            budget: self.budget.clone(),
        })
    }
    pub fn bytes(&self, filename: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.create(filename)?;
        file.write_all(bytes)?;
        file.finish()
    }
    pub fn header(&self, filename: &str, bytes: &[u8]) -> io::Result<()> {
        self.budget.borrow_mut().header(bytes.len())?;
        self.bytes(filename, bytes)
    }
    pub fn packet(&self, filename: &str, bytes: &[u8]) -> io::Result<()> {
        self.budget.borrow_mut().packet()?;
        self.bytes(filename, bytes)
    }
    pub fn json(&self, filename: &str, value: &impl Serialize) -> io::Result<()> {
        let mut file = self.create(filename)?;
        serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.finish()
    }
    /// Caller invokes only after independent decoder AND actual worker-Closed
    /// assertions. No success receipt exists for an unfinished/failing fixture.
    pub fn success(&self, value: &impl Serialize) -> io::Result<()> {
        self.json("acceptance.json", value)
    }
}
pub(crate) struct BoundedFile {
    file: File,
    budget: Rc<RefCell<Budget>>,
}
impl BoundedFile {
    pub fn json_line(&mut self, value: &impl Serialize) -> io::Result<()> {
        serde_json::to_writer(&mut *self, value).map_err(io::Error::other)?;
        self.write_all(b"\n")?;
        self.flush()
    }
    pub fn finish(mut self) -> io::Result<()> {
        self.flush()?;
        self.file.sync_all()
    }
}
impl Write for BoundedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Charge a complete requested write before I/O. Partial failures retain a
        // conservative charge and their partial files; no retry can exceed the cap.
        self.budget.borrow_mut().bytes(bytes.len())?;
        self.file.write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_boundaries_and_literal_components_are_checked_without_native_calls() {
        let mut budget = Budget::default();
        budget.bytes(MAX_BYTES).unwrap();
        assert!(budget.bytes(1).is_err());
        let mut budget = Budget::default();
        budget.header(MAX_HEADERS).unwrap();
        assert!(budget.header(1).is_err());
        let mut budget = Budget::default();
        for _ in 0..MAX_PACKETS {
            budget.packet().unwrap();
        }
        assert!(budget.packet().is_err());
        for invalid in [
            "",
            ".",
            "..",
            "../outside",
            "C:/outside",
            "packet/1",
            "packet\\1",
            "trailing.",
        ] {
            assert!(name(invalid).is_err());
        }
        name("au-00001.aac").unwrap();
    }
    #[test]
    fn create_new_preserves_existing_files_and_refuses_reused_run_directory() {
        static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "skylive-packet-evidence-policy-{}-{}",
            std::process::id(),
            ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let directory = Directory::new(path.clone()).unwrap();
        directory.bytes("packet.bin", b"original").unwrap();
        assert!(directory.bytes("packet.bin", b"replacement").is_err());
        assert_eq!(fs::read(path.join("packet.bin")).unwrap(), b"original");
        assert!(Directory::new(path.clone()).is_err());
        assert!(!path.join("acceptance.json").exists());
        drop(directory);
        fs::remove_dir_all(path).unwrap();
    }
}
