//! Win32 file I/O mapped onto Bionic, rooted at the game directory.
//! Windows paths the game uses (relative to its install dir, or absolute
//! `C:\...`) are translated to `<game_dir>/<relative>`. Handles are small
//! integers the guest sees; the host holds the real FILE*.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const GENERIC_READ: u32 = 0x8000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const OPEN_EXISTING: u32 = 3;
pub const CREATE_ALWAYS: u32 = 2;
pub const OPEN_ALWAYS: u32 = 4;
pub const CREATE_NEW: u32 = 1;
pub const TRUNCATE_EXISTING: u32 = 5;
pub const INVALID_HANDLE: u64 = u64::MAX;

struct Handle {
    file: File,
    path: PathBuf,
}

pub struct FileTable {
    game_dir: PathBuf,
    next: AtomicU64,
    handles: Mutex<HashMap<u64, Handle>>,
}

impl FileTable {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        Self {
            game_dir: game_dir.into(),
            next: AtomicU64::new(0x100),
            handles: Mutex::new(HashMap::new()),
        }
    }

    /// Translate a Windows path into a host path under the game directory.
    pub fn translate(&self, win_path: &str) -> PathBuf {
        let mut p = win_path.replace('\\', "/");
        // Strip a drive letter (C: etc.) and any leading slashes.
        if p.len() >= 2 && p.as_bytes()[1] == b':' {
            p = p[2..].to_string();
        }
        let p = p.trim_start_matches('/');
        self.game_dir.join(p)
    }

    pub fn create(&self, win_path: &str, access: u32, disposition: u32) -> u64 {
        let path = self.translate(win_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let read = access & GENERIC_READ != 0;
        let write = access & GENERIC_WRITE != 0;
        let mut opts = OpenOptions::new();
        opts.read(read || !write).write(write);
        match disposition {
            CREATE_ALWAYS | CREATE_NEW => { opts.create(true).truncate(true); }
            OPEN_ALWAYS => { opts.create(true); }
            TRUNCATE_EXISTING => { opts.truncate(true); }
            _ => {}
        }
        match opts.open(&path) {
            Ok(file) => {
                let mut h = self.handles.lock().unwrap();
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                h.insert(id, Handle { file, path });
                id
            }
            Err(_) => INVALID_HANDLE,
        }
    }

    pub fn read(&self, handle: u64, buf: &mut [u8]) -> u32 {
        let mut h = self.handles.lock().unwrap();
        if let Some(e) = h.get_mut(&handle) {
            match e.file.read(buf) {
                Ok(n) => n as u32,
                Err(_) => 0,
            }
        } else {
            0
        }
    }

    pub fn write(&self, handle: u64, buf: &[u8]) -> u32 {
        let mut h = self.handles.lock().unwrap();
        if let Some(e) = h.get_mut(&handle) {
            match e.file.write(buf) {
                Ok(n) => n as u32,
                Err(_) => 0,
            }
        } else {
            0
        }
    }

    pub fn seek(&self, handle: u64, off: i64, whence: u32) -> u64 {
        let mut h = self.handles.lock().unwrap();
        if let Some(e) = h.get_mut(&handle) {
            let from = match whence { 0 => SeekFrom::Start(off as u64), 1 => SeekFrom::Current(off), _ => SeekFrom::End(off) };
            e.file.seek(from).unwrap_or(0)
        } else {
            0
        }
    }

    pub fn size(&self, handle: u64) -> u64 {
        let h = self.handles.lock().unwrap();
        h.get(&handle).and_then(|e| e.file.metadata().ok()).map(|m| m.len()).unwrap_or(0)
    }

    pub fn close(&self, handle: u64) -> bool {
        self.handles.lock().unwrap().remove(&handle).is_some()
    }

    pub fn exists(&self, win_path: &str) -> bool {
        self.translate(win_path).exists()
    }
}
