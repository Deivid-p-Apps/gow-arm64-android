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
    finds: Mutex<HashMap<u64, (Vec<PathBuf>, usize)>>,
}

impl FileTable {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        Self {
            game_dir: game_dir.into(),
            next: AtomicU64::new(0x100),
            handles: Mutex::new(HashMap::new()),
            finds: Mutex::new(HashMap::new()),
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

    /// Minimal FindFirstFile: list entries matching a simple pattern prefix.
    pub fn find_first(&self, win_path: &str) -> u64 {
        let dir = self.translate(win_path);
        let (dir, pat) = match (dir.parent(), dir.file_name()) {
            (Some(d), Some(f)) => (d.to_path_buf(), f.to_string_lossy().replace('*', "")),
            _ => (dir.clone(), String::new()),
        };
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        let matches: Vec<PathBuf> = entries.into_iter().filter(|p| {
            pat.is_empty() || p.file_name().map(|n| n.to_string_lossy().starts_with(&pat)).unwrap_or(false)
        }).collect();
        let mut finds = self.finds.lock().unwrap();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        finds.insert(id, (matches, 0));
        id
    }

    /// Fill WIN32_FIND_DATAA at `out` from the next match; returns 1 on success.
    pub fn find_next(&self, find: u64, out: u64) -> u32 {
        let mut finds = self.finds.lock().unwrap();
        let entry = match finds.get_mut(&find) {
            Some(e) => e,
            None => return 0,
        };
        if entry.1 >= entry.0.len() { return 0; }
        let path = entry.0[entry.1].clone();
        entry.1 += 1;
        if out != 0 {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let meta = std::fs::metadata(&path).ok();
            unsafe {
                // cFileName at offset 0x2c in WIN32_FIND_DATAA (44 bytes).
                let namep = (out as *mut u8).add(0x2c);
                let nb = name.as_bytes();
                let n = nb.len().min(259);
                std::ptr::copy_nonoverlapping(nb.as_ptr(), namep, n);
                *namep.add(n) = 0;
                // dwFileAttributes at 0
                let attr = if meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) { 0x10u32 } else { 0x80 };
                *(out as *mut u32) = attr;
                // nFileSizeHigh/Low at 0x1c/0x20
                if let Some(m) = meta {
                    *((out as *mut u8).add(0x1c) as *mut u32) = (m.len() >> 32) as u32;
                    *((out as *mut u8).add(0x20) as *mut u32) = (m.len() & 0xffff_ffff) as u32;
                }
            }
        }
        let _ = self.next.fetch_add(0, Ordering::Relaxed);
        1
    }

    pub fn find_close(&self, find: u64) {
        self.finds.lock().unwrap().remove(&find);
    }

    /// Return a stable pointer to a NUL-terminated command line string.
    pub fn cmdline_ptr(&self, wide: bool) -> u64 {
        let s = "GoW.exe\0";
        if wide {
            let mut v: Vec<u16> = s.bytes().map(|b| b as u16).collect();
            v.push(0);
            let b: Vec<u8> = v.iter().flat_map(|c| c.to_le_bytes()).collect();
            self.store_static(b)
        } else {
            self.store_static(s.as_bytes().to_vec())
        }
    }

    fn store_static(&self, b: Vec<u8>) -> u64 {
        // Leak a stable buffer so the pointer stays valid for the process life.
        let mut v = b;
        v.reserve(1);
        let ptr = v.as_ptr() as u64;
        std::mem::forget(v);
        ptr
    }
}
