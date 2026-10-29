//! Compact in-memory file index plus the parallel drive scanner that fills it.
//!
//! Names live in string arenas (original and, only where it differs, lowercase)
//! so each entry is a small fixed-size record. Folders form a tree of parent
//! pointers; full paths are rebuilt on demand instead of being stored.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};

pub const F_DIR: u8 = 1;
pub const F_HIDDEN: u8 = 2;
pub const F_SYSTEM: u8 = 4;
/// The name is already lowercase, so it has no copy in the lowercase arena.
const F_LOWER_SAME: u8 = 8;

/// Marks a missing parent or entry reference.
pub const NONE: u32 = u32::MAX;

const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// Folders that are never worth descending into.
const SKIP_DIRS: &[&str] = &["$recycle.bin", "system volume information", "$winreagent"];

/// Top-level folders that belong to Windows or installed programs rather than the user.
const SYSTEM_ROOT_DIRS: &[&str] = &[
    "windows",
    "windows.old",
    "program files",
    "program files (x86)",
    "programdata",
    "perflogs",
    "recovery",
    "msocache",
    "intel",
    "amd",
    "nvidia",
    "drivers",
    "onedrivetemp",
    "$windows.~bt",
    "$windows.~ws",
    "$sysreset",
    "$getcurrent",
];

/// Whether a folder counts as a system location; everything inside it is treated as system too.
fn is_system_dir(lower: &str, at_root: bool) -> bool {
    // Per-user application data (C:\Users\<name>\AppData) is system territory wherever it lives.
    lower == "appdata" || (at_root && SYSTEM_ROOT_DIRS.contains(&lower))
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub name_off: u32,
    pub lower_off: u32,
    /// The directory (index into `Index::dirs`) that contains this entry.
    pub parent: u32,
    pub name_len: u16,
    pub lower_len: u16,
    pub flags: u8,
    pub size: u64,
    /// Last write time, seconds since the Unix epoch.
    pub modified: u32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.flags & F_DIR != 0
    }
}

/// A scanned directory: its parent directory and its own entry in that parent.
#[derive(Clone, Copy)]
pub struct DirRec {
    pub parent: u32,
    pub entry: u32,
}

pub struct Root {
    pub dir: u32,
    pub path: Box<str>,
    pub lower: Box<str>,
}

#[derive(Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    pub names: String,
    pub lower: String,
    pub dirs: Vec<DirRec>,
    pub roots: Vec<Root>,
}

impl Index {
    pub fn name(&self, e: &Entry) -> &str {
        let s = e.name_off as usize;
        &self.names[s..s + e.name_len as usize]
    }

    pub fn name_lower(&self, e: &Entry) -> &str {
        if e.flags & F_LOWER_SAME != 0 {
            return self.name(e);
        }
        let s = e.lower_off as usize;
        &self.lower[s..s + e.lower_len as usize]
    }

    fn root(&self, dir: u32) -> Option<&Root> {
        self.roots.iter().find(|r| r.dir == dir)
    }

    /// The last path component of a directory (`C:\` for a drive root).
    pub fn dir_name(&self, dir: u32, lower: bool) -> &str {
        let rec = self.dirs[dir as usize];
        if rec.entry != NONE {
            let e = &self.entries[rec.entry as usize];
            return if lower { self.name_lower(e) } else { self.name(e) };
        }
        match self.root(dir) {
            Some(r) if lower => &r.lower,
            Some(r) => &r.path,
            None => "…",
        }
    }

    pub fn dir_path_into(&self, dir: u32, out: &mut String, lower: bool) {
        self.dir_path_rec(dir, out, lower, 0);
    }

    fn dir_path_rec(&self, dir: u32, out: &mut String, lower: bool, depth: u32) {
        let parent = self.dirs[dir as usize].parent;
        // The depth cap only guards against a corrupted cache containing a cycle.
        if parent != NONE && depth < 1024 {
            self.dir_path_rec(parent, out, lower, depth + 1);
            if !out.ends_with('\\') {
                out.push('\\');
            }
        }
        out.push_str(self.dir_name(dir, lower));
    }

    pub fn dir_path(&self, dir: u32) -> String {
        let mut out = String::new();
        self.dir_path_into(dir, &mut out, false);
        out
    }

    pub fn full_path_into(&self, e: &Entry, out: &mut String, lower: bool) {
        self.dir_path_into(e.parent, out, lower);
        if !out.ends_with('\\') {
            out.push('\\');
        }
        out.push_str(if lower { self.name_lower(e) } else { self.name(e) });
    }

    pub fn full_path(&self, e: &Entry) -> String {
        let mut out = String::new();
        self.full_path_into(e, &mut out, false);
        out
    }

    fn dir_mut(&mut self, dir: u32) -> &mut DirRec {
        let i = dir as usize;
        if self.dirs.len() <= i {
            self.dirs.resize(i + 1, DirRec { parent: NONE, entry: NONE });
        }
        &mut self.dirs[i]
    }

    /// Releases the spare capacity left over from growing the arenas during a scan.
    fn shrink(&mut self) {
        self.entries.shrink_to_fit();
        self.names.shrink_to_fit();
        self.lower.shrink_to_fit();
        self.dirs.shrink_to_fit();
    }

    fn add_batch(&mut self, batch: Batch) {
        self.dir_mut(batch.dir).parent = batch.parent;
        if let Some(path) = batch.root {
            let lower = path.to_lowercase().into_boxed_str();
            self.roots.push(Root { dir: batch.dir, path: path.into_boxed_str(), lower });
        }
        self.entries.reserve(batch.entries.len());
        for raw in batch.entries {
            let id = self.entries.len() as u32;
            if raw.dir_id != NONE {
                *self.dir_mut(raw.dir_id) = DirRec { parent: batch.dir, entry: id };
            }
            let name_off = self.names.len() as u32;
            self.names.push_str(&raw.name);
            let (lower_off, lower_len, same) = match &raw.lower {
                Some(l) => {
                    let off = self.lower.len() as u32;
                    self.lower.push_str(l);
                    (off, l.len() as u16, 0)
                }
                None => (0, 0, F_LOWER_SAME),
            };
            self.entries.push(Entry {
                name_off,
                lower_off,
                parent: batch.dir,
                name_len: raw.name.len() as u16,
                lower_len,
                flags: raw.flags | same,
                size: raw.size,
                modified: raw.modified,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state between the UI, the indexer and the search worker.

pub struct Shared {
    pub index: RwLock<Index>,
    /// Bumped whenever the index is replaced wholesale (entry ids become invalid).
    pub version: AtomicU64,
    pub indexing: AtomicBool,
    pub scanned: AtomicU64,
    pub status: Mutex<String>,
    pub last_scan: Mutex<Option<(Instant, Duration)>>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            index: RwLock::new(Index::default()),
            version: AtomicU64::new(0),
            indexing: AtomicBool::new(false),
            scanned: AtomicU64::new(0),
            status: Mutex::new(String::new()),
            last_scan: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Scanning

struct RawEntry {
    name: String,
    /// Lowercase name, only when it differs from `name`.
    lower: Option<String>,
    flags: u8,
    size: u64,
    modified: u32,
    /// Directory id assigned to this entry if it is a folder that gets scanned.
    dir_id: u32,
}

/// The contents of one directory, produced by a scan worker.
struct Batch {
    dir: u32,
    parent: u32,
    root: Option<String>,
    entries: Vec<RawEntry>,
}

struct ScanCtx {
    tx: Sender<Batch>,
    next_dir: AtomicU32,
    shared: Arc<Shared>,
}

fn filetime_to_unix(ft: u64) -> u32 {
    ((ft / 10_000_000) as i64 - 11_644_473_600).clamp(0, u32::MAX as i64) as u32
}

/// `inherited` holds the hidden/system flags of the folder being listed, which its contents inherit.
fn scan_dir<'s>(scope: &rayon::Scope<'s>, ctx: &'s ScanCtx, path: PathBuf, dir: u32, parent: u32, inherited: u8) {
    let Ok(read) = fs::read_dir(&path) else { return };
    let at_root = parent == NONE;
    let mut entries = Vec::new();
    for item in read.flatten() {
        // On Windows this metadata comes straight from FindNextFile: no extra syscall.
        let Ok(meta) = item.metadata() else { continue };
        let attrs = meta.file_attributes();
        let name = item.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        let is_dir = meta.is_dir();
        let mut flags = inherited;
        if is_dir {
            flags |= F_DIR;
        }
        if attrs & ATTR_HIDDEN != 0 {
            flags |= F_HIDDEN;
        }
        if attrs & ATTR_SYSTEM != 0 || (is_dir && is_system_dir(&lower, at_root)) {
            flags |= F_SYSTEM;
        }
        let mut dir_id = NONE;
        // Junctions and symlinks are listed but never followed (avoids cycles and duplicates).
        if is_dir && attrs & ATTR_REPARSE_POINT == 0 && !SKIP_DIRS.contains(&lower.as_str()) {
            dir_id = ctx.next_dir.fetch_add(1, Ordering::Relaxed);
            let sub = item.path();
            let inherit = flags & (F_HIDDEN | F_SYSTEM);
            scope.spawn(move |s| scan_dir(s, ctx, sub, dir_id, dir, inherit));
        }
        entries.push(RawEntry {
            lower: (lower != name).then_some(lower),
            name,
            flags,
            size: if is_dir { 0 } else { meta.file_size() },
            modified: filetime_to_unix(meta.last_write_time()),
            dir_id,
        });
    }
    ctx.shared.scanned.fetch_add(entries.len() as u64, Ordering::Relaxed);
    let root = (parent == NONE).then(|| path.to_string_lossy().into_owned());
    let _ = ctx.tx.send(Batch { dir, parent, root, entries });
}

/// Drains scan batches into an index, holding the lock only briefly at a time.
fn collect(rx: Receiver<Batch>, mut apply: impl FnMut(&mut dyn FnMut(&mut Index))) {
    while let Ok(first) = rx.recv() {
        let mut pending = vec![first];
        while pending.len() < 512 {
            match rx.try_recv() {
                Ok(b) => pending.push(b),
                Err(_) => break,
            }
        }
        apply(&mut |idx| {
            for b in pending.drain(..) {
                idx.add_batch(b);
            }
        });
    }
}

fn scan_roots(roots: &[PathBuf], shared: &Arc<Shared>, apply: impl FnMut(&mut dyn FnMut(&mut Index)) + Send) {
    let (tx, rx) = crossbeam_channel::unbounded::<Batch>();
    // Directory listing is I/O bound, so use more threads than cores.
    let threads = (std::thread::available_parallelism().map_or(4, |n| n.get()) * 2).clamp(4, 32);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("scan-{i}"))
        .build()
        .expect("scan thread pool");
    let ctx = ScanCtx { tx, next_dir: AtomicU32::new(roots.len() as u32), shared: shared.clone() };
    std::thread::scope(|s| {
        s.spawn(move || collect(rx, apply));
        pool.scope(|scope| {
            for (i, root) in roots.iter().enumerate() {
                let root = root.clone();
                let ctx = &ctx;
                scope.spawn(move |s| scan_dir(s, ctx, root, i as u32, NONE, 0));
            }
        });
        // Closes the channel so the collector finishes.
        drop(ctx);
    });
}

/// Loads the cached index (if any), then rescans `roots` in the background.
///
/// Without a cache the scan fills the shared index progressively so results
/// appear immediately; with a cache the fresh index is swapped in when done.
pub fn start(shared: Arc<Shared>, roots: Vec<PathBuf>, use_cache: bool, repaint: impl Fn() + Send + 'static) {
    if shared.indexing.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("indexer".into())
        .spawn(move || {
            let cache = cache_path();
            let mut have_index = !shared.index.read().entries.is_empty();
            if use_cache && !have_index {
                if let Some(path) = &cache {
                    *shared.status.lock() = "Loading index…".into();
                    repaint();
                    if let Ok(idx) = load(path) {
                        *shared.index.write() = idx;
                        shared.version.fetch_add(1, Ordering::SeqCst);
                        have_index = true;
                    }
                }
            }

            let started = Instant::now();
            shared.scanned.store(0, Ordering::Relaxed);
            let names: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
            *shared.status.lock() = format!("Scanning {}", names.join("  "));
            repaint();

            if have_index {
                let mut fresh = Index::default();
                scan_roots(&roots, &shared, |f| f(&mut fresh));
                fresh.shrink();
                *shared.index.write() = fresh;
                shared.version.fetch_add(1, Ordering::SeqCst);
            } else {
                // Progressive: entries are only appended, so existing result ids stay valid.
                *shared.index.write() = Index::default();
                shared.version.fetch_add(1, Ordering::SeqCst);
                scan_roots(&roots, &shared, |f| f(&mut shared.index.write()));
                shared.index.write().shrink();
            }

            *shared.last_scan.lock() = Some((Instant::now(), started.elapsed()));
            *shared.status.lock() = String::new();
            shared.indexing.store(false, Ordering::SeqCst);
            repaint();

            if let Some(path) = &cache {
                let _ = save(&shared.index.read(), path);
            }
        })
        .expect("spawn indexer");
}

// ---------------------------------------------------------------------------
// On-disk cache: a flat little-endian dump of the arenas and records.

const MAGIC: &[u8; 8] = b"FSIDX\x00\x00\x03";

pub fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("FileSearch").join("index.bin"))
}

fn save(idx: &Index, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&(idx.entries.len() as u64).to_le_bytes())?;
        for e in &idx.entries {
            w.write_all(&e.name_off.to_le_bytes())?;
            w.write_all(&e.lower_off.to_le_bytes())?;
            w.write_all(&e.parent.to_le_bytes())?;
            w.write_all(&e.name_len.to_le_bytes())?;
            w.write_all(&e.lower_len.to_le_bytes())?;
            w.write_all(&[e.flags])?;
            w.write_all(&e.size.to_le_bytes())?;
            w.write_all(&e.modified.to_le_bytes())?;
        }
        write_str(&mut w, &idx.names)?;
        write_str(&mut w, &idx.lower)?;
        w.write_all(&(idx.dirs.len() as u64).to_le_bytes())?;
        for d in &idx.dirs {
            w.write_all(&d.parent.to_le_bytes())?;
            w.write_all(&d.entry.to_le_bytes())?;
        }
        w.write_all(&(idx.roots.len() as u64).to_le_bytes())?;
        for r in &idx.roots {
            w.write_all(&r.dir.to_le_bytes())?;
            write_str(&mut w, &r.path)?;
        }
        w.flush()?;
    }
    fs::rename(tmp, path)
}

fn write_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    w.write_all(&(s.len() as u64).to_le_bytes())?;
    w.write_all(s.as_bytes())
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.buf.len() < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    fn arr<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn string(&mut self) -> io::Result<String> {
        let n = self.u64()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| io::ErrorKind::InvalidData.into())
    }
}

fn load(path: &Path) -> io::Result<Index> {
    let data = fs::read(path)?;
    let mut r = Reader { buf: &data };
    if r.take(MAGIC.len())? != MAGIC {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let n = r.u64()? as usize;
    let mut entries = Vec::with_capacity(n.min(data.len() / 29));
    for _ in 0..n {
        entries.push(Entry {
            name_off: r.u32()?,
            lower_off: r.u32()?,
            parent: r.u32()?,
            name_len: u16::from_le_bytes(r.arr()?),
            lower_len: u16::from_le_bytes(r.arr()?),
            flags: r.arr::<1>()?[0],
            size: r.u64()?,
            modified: r.u32()?,
        });
    }
    let names = r.string()?;
    let lower = r.string()?;
    let dir_count = r.u64()? as usize;
    let mut dirs = Vec::with_capacity(dir_count.min(data.len() / 8));
    for _ in 0..dir_count {
        dirs.push(DirRec { parent: r.u32()?, entry: r.u32()? });
    }
    let root_count = r.u64()? as usize;
    let mut roots = Vec::new();
    for _ in 0..root_count {
        let dir = r.u32()?;
        let path = r.string()?;
        let lower = path.to_lowercase().into_boxed_str();
        roots.push(Root { dir, path: path.into_boxed_str(), lower });
    }
    // Validate references so a corrupt cache can never cause an out-of-bounds slice.
    let in_dirs = |d: u32| d == NONE || (d as usize) < dirs.len();
    let ok = entries.iter().all(|e| {
        (e.parent as usize) < dirs.len()
            && names.get(e.name_off as usize..e.name_off as usize + e.name_len as usize).is_some()
            && (e.flags & F_LOWER_SAME != 0
                || lower.get(e.lower_off as usize..e.lower_off as usize + e.lower_len as usize).is_some())
    }) && dirs.iter().enumerate().all(|(i, d)| in_dirs(d.parent) && d.parent != i as u32 && (d.entry == NONE || (d.entry as usize) < entries.len()))
        && roots.iter().all(|r| (r.dir as usize) < dirs.len());
    if !ok {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Index { entries, names, lower, dirs, roots })
}
