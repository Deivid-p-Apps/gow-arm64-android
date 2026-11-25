//! Win32 file I/O mapped onto Bionic, rooted at the game directory.
//! Windows paths the game uses (relative to its install dir, or absolute
//! `C:\...`) are translated to `<game_dir>/<relative>`. Handles are small
//! integers the guest sees; the host holds the real FILE*.
//!
//! Mappings (D016): `CreateFileMapping` mmaps the file (MAP_PRIVATE) instead of
//! reading it whole into RAM — GoW pack files are gigabytes and the eager copy
//! is what ate the device's memory. Views are tracked so `UnmapViewOfFile`
//! really unmaps. Falls back to the old eager Vec when mmap is unavailable.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[cfg(unix)]
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

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

struct Mapping {
    base: u64,
    len: usize,
    /// Kept open while the mapping exists (keeps the mmap fd valid). Closing
    /// the mapping handle drops the File; MAP_PRIVATE pages stay valid.
    file: Option<File>,
    /// Fallback backing when mmap is unavailable.
    _buf: Vec<u8>,
    is_mmap: bool,
}

struct ViewInfo {
    len: usize,
    is_mmap: bool,
}

pub struct FileTable {
    game_dir: PathBuf,
    next: AtomicU64,
    handles: Mutex<HashMap<u64, Handle>>,
    mappings: Mutex<HashMap<u64, Mapping>>,
    views: Mutex<HashMap<u64, ViewInfo>>,
    finds: Mutex<HashMap<u64, (Vec<PathBuf>, usize)>>,
}

impl FileTable {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        Self {
            game_dir: game_dir.into(),
            next: AtomicU64::new(0x100),
            handles: Mutex::new(HashMap::new()),
            mappings: Mutex::new(HashMap::new()),
            views: Mutex::new(HashMap::new()),
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

    /// The configured game directory (host path). Used by handlers that must
    /// materialize engine data folders (e.g. SHGetKnownFolderPath).
    pub fn game_dir(&self) -> &std::path::Path {
        &self.game_dir
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
        // A mapping handle: close the fd but keep the mapping alive (Windows
        // allows CloseHandle on a mapping with open views; MAP_PRIVATE pages
        // stay valid without the fd).
        {
            let mut m = self.mappings.lock().unwrap();
            if let Some(mm) = m.get_mut(&handle) {
                mm.file = None;
                return true;
            }
        }
        self.handles.lock().unwrap().remove(&handle).is_some()
    }

    /// Create a file mapping of `size` bytes (0 = whole file) on `file_handle`.
    /// Returns a pseudo mapping handle (>0) or INVALID_HANDLE.
    ///
    /// The file is `mmap`ed (MAP_PRIVATE) instead of read into RAM: pages
    /// arrive from the page cache on demand (D016). Falls back to the eager
    /// Vec when mmap is unavailable or fails.
    pub fn create_mapping(&self, file_handle: u64, size: u64) -> u64 {
        let h = self.handles.lock().unwrap();
        let e = match h.get(&file_handle) {
            Some(e) => e,
            None => return INVALID_HANDLE,
        };
        let path = e.path.clone();
        drop(h);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(_) => return INVALID_HANDLE,
        };
        let fsize = file.metadata().map(|m| m.len()).unwrap_or(0);
        let n = if size == 0 || size > fsize { fsize } else { size } as usize;
        let id = self.next.fetch_add(1, Ordering::Relaxed);

        #[cfg(unix)]
        {
            if n > 0 {
                const PROT_READ: i32 = 1;
                const PROT_WRITE: i32 = 2;
                const MAP_PRIVATE: i32 = 2;
                let p = unsafe {
                    mmap(std::ptr::null_mut(), n, PROT_READ | PROT_WRITE, MAP_PRIVATE, file.as_raw_fd(), 0)
                };
                // MAP_FAILED is (void*)-1.
                if p as isize != -1 && !p.is_null() {
                    self.mappings.lock().unwrap().insert(id, Mapping {
                        base: p as u64, len: n, file: Some(file), _buf: Vec::new(), is_mmap: true,
                    });
                    return id;
                }
            }
        }

        // Fallback (non-unix or mmap failure): eager copy, as before.
        drop(file);
        match std::fs::read(&path) {
            Ok(data) => {
                let mut buf = data;
                buf.truncate(n);
                let ptr = buf.as_ptr() as u64;
                self.mappings.lock().unwrap().insert(id, Mapping {
                    base: ptr, len: n, file: None, _buf: buf, is_mmap: false,
                });
                id
            }
            Err(_) => INVALID_HANDLE,
        }
    }

    /// Return the base pointer for a mapping and a view offset/size. The view
    /// is registered so UnmapViewOfFile can release it.
    pub fn map_view(&self, mapping: u64, offset: u64, size: u64) -> u64 {
        let m = self.mappings.lock().unwrap();
        let mm = match m.get(&mapping) {
            Some(mm) => mm,
            None => return 0,
        };
        if offset as usize >= mm.len { return 0; }
        let base = mm.base + offset;
        let avail = mm.len - offset as usize;
        let vlen = if size == 0 { avail } else { (size as usize).min(avail) };
        let is_mmap = mm.is_mmap;
        drop(m);
        self.views.lock().unwrap().insert(base, ViewInfo { len: vlen, is_mmap });
        base
    }

    pub fn unmap_view(&self, base: u64) -> bool {
        if let Some(v) = self.views.lock().unwrap().remove(&base) {
            #[cfg(unix)]
            if v.is_mmap {
                let len = (v.len + 0xfff) & !0xfff;
                unsafe { munmap(base as *mut u8, len); }
            }
            return true;
        }
        // Not a registered view (e.g. a Vec-backed mapping): the buffer stays
        // alive with the Mapping entry, matching the old behaviour.
        true
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
