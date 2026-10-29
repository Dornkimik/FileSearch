//! Query model, compilation of UI filters into a fast matcher, and the
//! background search worker.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use memchr::memmem::Finder;
use parking_lot::Mutex;
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};

use crate::index::{Entry, F_HIDDEN, F_SYSTEM, Index, NONE, Shared};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Contains,
    Wildcard,
    Regex,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    All,
    Files,
    Folders,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeUnit {
    Kb,
    #[default]
    Mb,
    Gb,
}

impl SizeUnit {
    pub const ALL: [SizeUnit; 3] = [SizeUnit::Kb, SizeUnit::Mb, SizeUnit::Gb];
    pub fn label(self) -> &'static str {
        match self {
            SizeUnit::Kb => "KB",
            SizeUnit::Mb => "MB",
            SizeUnit::Gb => "GB",
        }
    }
    fn bytes(self) -> f64 {
        match self {
            SizeUnit::Kb => 1024.0,
            SizeUnit::Mb => 1024.0 * 1024.0,
            SizeUnit::Gb => 1024.0 * 1024.0 * 1024.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DatePreset {
    #[default]
    Any,
    Today,
    Week,
    Month,
    Year,
    OlderThanYear,
}

impl DatePreset {
    pub const ALL: [DatePreset; 6] = [
        DatePreset::Any,
        DatePreset::Today,
        DatePreset::Week,
        DatePreset::Month,
        DatePreset::Year,
        DatePreset::OlderThanYear,
    ];
    pub fn label(self) -> &'static str {
        match self {
            DatePreset::Any => "Any time",
            DatePreset::Today => "Last 24 hours",
            DatePreset::Week => "Last 7 days",
            DatePreset::Month => "Last 30 days",
            DatePreset::Year => "Last 12 months",
            DatePreset::OlderThanYear => "Older than a year",
        }
    }
}

pub struct Category {
    pub name: &'static str,
    pub exts: &'static [&'static str],
}

pub const CATEGORIES: &[Category] = &[
    Category {
        name: "Documents",
        exts: &[
            "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp",
            "epub", "pages", "numbers", "key", "one", "tex",
        ],
    },
    Category {
        name: "Images",
        exts: &[
            "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "tif", "tiff", "ico", "heic", "heif", "avif", "raw",
            "cr2", "nef", "arw", "dng", "psd",
        ],
    },
    Category {
        name: "Audio",
        exts: &["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "aiff", "mid", "midi"],
    },
    Category {
        name: "Video",
        exts: &["mp4", "mkv", "avi", "mov", "wmv", "webm", "flv", "m4v", "mpg", "mpeg", "3gp", "mts"],
    },
    Category {
        name: "Archives",
        exts: &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "tgz", "iso", "cab", "img"],
    },
    Category {
        name: "Code",
        exts: &[
            "rs", "py", "js", "ts", "jsx", "tsx", "c", "h", "cpp", "hpp", "cs", "java", "kt", "go", "rb", "php",
            "swift", "lua", "html", "css", "scss", "json", "toml", "yaml", "yml", "xml", "sql", "sh", "ps1", "ipynb",
        ],
    },
    Category {
        name: "Programs",
        exts: &["exe", "msi", "msix", "appx", "bat", "cmd", "com", "lnk"],
    },
];

/// Raw filter state exactly as the user entered it. Compiled on the worker.
#[derive(Clone, PartialEq, Default)]
pub struct Filters {
    pub text: String,
    pub mode: Mode,
    pub case_sensitive: bool,
    pub match_path: bool,
    pub kind: Kind,
    pub categories: [bool; 7],
    pub extensions: String,
    pub min_size: String,
    pub max_size: String,
    pub size_unit: SizeUnit,
    pub modified: DatePreset,
    pub location: String,
    pub include_hidden: bool,
    pub include_system: bool,
}

impl Filters {
    /// Number of filters beyond the text query that are currently narrowing results.
    pub fn active_count(&self) -> usize {
        [
            self.kind != Kind::All,
            self.categories.iter().any(|c| *c) || !self.extensions.trim().is_empty(),
            !self.min_size.trim().is_empty() || !self.max_size.trim().is_empty(),
            self.modified != DatePreset::Any,
            !self.location.trim().is_empty(),
        ]
        .iter()
        .filter(|b| **b)
        .count()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Folder,
    Size,
    Modified,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
}

// ---------------------------------------------------------------------------
// Compilation

struct Token {
    finder: Finder<'static>,
    /// The token contains a separator, so it can only be tested against a full path.
    spans_dirs: bool,
    /// In path mode: whether each directory's path contains the token.
    dir_hit: Vec<bool>,
}

enum Text {
    Any,
    Tokens { include: Vec<Token>, exclude: Vec<Token> },
    Regex(Regex),
}

struct Compiled {
    text: Text,
    lowercase: bool,
    match_path: bool,
    kind: Kind,
    exts: Option<HashSet<String>>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    after: Option<u32>,
    before: Option<u32>,
    /// Per directory: whether it lies inside the location filter.
    location: Option<Vec<bool>>,
    include_hidden: bool,
    include_system: bool,
}

fn parse_size(s: &str, unit: SizeUnit, which: &str) -> Result<Option<u64>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let v: f64 = s.replace(',', ".").parse().map_err(|_| format!("Invalid {which} size \"{s}\""))?;
    if v < 0.0 {
        return Err(format!("Invalid {which} size \"{s}\""));
    }
    Ok(Some((v * unit.bytes()) as u64))
}

fn wildcard_to_regex(pattern: &str) -> String {
    let has_wild = pattern.contains(['*', '?']);
    let mut re = String::from(if has_wild { "^" } else { "" });
    for c in pattern.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    if has_wild {
        re.push('$');
    }
    re
}

/// Computes a value for every directory from its parent's value, top-down.
/// `f` receives `None` for drive roots (and orphaned folders).
fn fold_dirs<T: Copy>(idx: &Index, f: impl Fn(Option<T>, u32) -> T) -> Vec<T> {
    let mut out: Vec<Option<T>> = vec![None; idx.dirs.len()];
    let mut chain = Vec::new();
    for d in 0..idx.dirs.len() as u32 {
        let mut cur = d;
        while cur != NONE && out[cur as usize].is_none() && chain.len() <= 1024 {
            chain.push(cur);
            cur = idx.dirs[cur as usize].parent;
        }
        let mut acc = if cur == NONE { None } else { out[cur as usize] };
        while let Some(x) = chain.pop() {
            let v = f(acc, x);
            out[x as usize] = Some(v);
            acc = Some(v);
        }
    }
    out.into_iter().map(|v| v.expect("every dir visited")).collect()
}

fn location_dirs(idx: &Index, location: &str) -> Vec<bool> {
    let parts: Vec<&str> = location.split('\\').filter(|s| !s.is_empty()).collect();
    let n = parts.len() as i32;
    // Value: how many leading components of `location` this directory's path has matched (-1: diverged).
    let state = fold_dirs(idx, |parent, d| {
        let name = idx.dir_name(d, true).trim_end_matches('\\');
        match parent {
            None => if parts.first() == Some(&name) { 1 } else { -1 },
            Some(k) if k < 0 || k == n => k,
            Some(k) => if parts[k as usize] == name { k + 1 } else { -1 },
        }
    });
    state.into_iter().map(|k| k == n).collect()
}

fn compile(f: &Filters, idx: &Index) -> Result<Compiled, String> {
    let raw = f.text.trim();
    let lowercase = !f.case_sensitive;
    let text = if raw.is_empty() {
        Text::Any
    } else {
        match f.mode {
            Mode::Contains => {
                let mut include = Vec::new();
                let mut exclude = Vec::new();
                for tok in raw.split_whitespace() {
                    let tok = if lowercase { tok.to_lowercase() } else { tok.to_string() };
                    let (list, tok) = match tok.strip_prefix('-') {
                        Some(rest) if !rest.is_empty() => (&mut exclude, rest.to_string()),
                        _ => (&mut include, tok),
                    };
                    let tok = tok.replace('/', "\\");
                    let finder = Finder::new(tok.as_bytes()).into_owned();
                    let spans_dirs = tok.contains('\\');
                    let dir_hit = if f.match_path && !spans_dirs {
                        fold_dirs(idx, |parent, d| {
                            parent.unwrap_or(false) || finder.find(idx.dir_name(d, lowercase).as_bytes()).is_some()
                        })
                    } else {
                        Vec::new()
                    };
                    list.push(Token { finder, spans_dirs, dir_hit });
                }
                Text::Tokens { include, exclude }
            }
            Mode::Wildcard | Mode::Regex => {
                let pattern = if f.mode == Mode::Wildcard { wildcard_to_regex(raw) } else { raw.to_string() };
                let re = RegexBuilder::new(&pattern)
                    .case_insensitive(lowercase)
                    .build()
                    .map_err(|e| match e {
                        regex::Error::Syntax(s) => s.lines().last().unwrap_or("Invalid pattern").to_string(),
                        e => e.to_string(),
                    })?;
                Text::Regex(re)
            }
        }
    };

    let mut exts: HashSet<String> = HashSet::new();
    for (i, on) in f.categories.iter().enumerate() {
        if *on {
            exts.extend(CATEGORIES[i].exts.iter().map(|e| e.to_string()));
        }
    }
    for e in f.extensions.split([',', ';', ' ']) {
        let e = e.trim().trim_start_matches("*.").trim_start_matches('.').to_lowercase();
        if !e.is_empty() {
            exts.insert(e);
        }
    }

    let now = chrono::Utc::now().timestamp();
    const DAY: i64 = 86_400;
    let ago = |days: i64| Some((now - days * DAY).max(0) as u32);
    let (after, before) = match f.modified {
        DatePreset::Any => (None, None),
        DatePreset::Today => (ago(1), None),
        DatePreset::Week => (ago(7), None),
        DatePreset::Month => (ago(30), None),
        DatePreset::Year => (ago(365), None),
        DatePreset::OlderThanYear => (None, ago(365)),
    };

    let location = f.location.trim().replace('/', "\\").to_lowercase();

    Ok(Compiled {
        text,
        lowercase,
        match_path: f.match_path,
        kind: f.kind,
        exts: (!exts.is_empty()).then_some(exts),
        min_size: parse_size(&f.min_size, f.size_unit, "minimum")?,
        max_size: parse_size(&f.max_size, f.size_unit, "maximum")?,
        after,
        before,
        location: (!location.is_empty()).then(|| location_dirs(idx, &location)),
        include_hidden: f.include_hidden,
        include_system: f.include_system,
    })
}

pub fn extension(name_lower: &str) -> &str {
    match name_lower.rfind('.') {
        Some(p) if p > 0 => &name_lower[p + 1..],
        _ => "",
    }
}

impl Compiled {
    fn matches(&self, idx: &Index, e: &Entry, buf: &mut String) -> bool {
        if (!self.include_hidden && e.flags & F_HIDDEN != 0) || (!self.include_system && e.flags & F_SYSTEM != 0) {
            return false;
        }
        let is_dir = e.is_dir();
        match self.kind {
            Kind::Files if is_dir => return false,
            Kind::Folders if !is_dir => return false,
            _ => {}
        }
        let size_filter = self.min_size.is_some() || self.max_size.is_some();
        if is_dir && (size_filter || self.exts.is_some()) {
            return false;
        }
        if self.min_size.is_some_and(|m| e.size < m) || self.max_size.is_some_and(|m| e.size > m) {
            return false;
        }
        if self.after.is_some_and(|t| e.modified < t) || self.before.is_some_and(|t| e.modified > t) {
            return false;
        }
        if let Some(exts) = &self.exts {
            if !exts.contains(extension(idx.name_lower(e))) {
                return false;
            }
        }
        if let Some(loc) = &self.location {
            if !loc[e.parent as usize] {
                return false;
            }
        }

        match &self.text {
            Text::Any => true,
            Text::Tokens { include, exclude } => {
                let name = if self.lowercase { idx.name_lower(e) } else { idx.name(e) }.as_bytes();
                let mut full_built = false;
                let mut hit = |t: &Token| -> bool {
                    if t.spans_dirs {
                        if !full_built {
                            buf.clear();
                            idx.full_path_into(e, buf, self.lowercase);
                            full_built = true;
                        }
                        return t.finder.find(buf.as_bytes()).is_some();
                    }
                    t.finder.find(name).is_some() || (self.match_path && t.dir_hit[e.parent as usize])
                };
                include.iter().all(&mut hit) && !exclude.iter().any(&mut hit)
            }
            Text::Regex(re) => {
                if self.match_path {
                    buf.clear();
                    idx.full_path_into(e, buf, false);
                    re.is_match(buf)
                } else {
                    re.is_match(idx.name(e))
                }
            }
        }
    }
}

/// Ranks directories in sorted-path order (component-wise), for sorting by folder.
fn folder_ranks(idx: &Index) -> Vec<u32> {
    let n = idx.dirs.len();
    // Children lists in compressed form: `kids[start[d]..start[d + 1]]`.
    let mut start = vec![0u32; n + 1];
    for d in &idx.dirs {
        if d.parent != NONE {
            start[d.parent as usize + 1] += 1;
        }
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut kids = vec![0u32; start[n] as usize];
    for (i, d) in idx.dirs.iter().enumerate() {
        if d.parent != NONE {
            kids[fill[d.parent as usize] as usize] = i as u32;
            fill[d.parent as usize] += 1;
        }
    }
    for d in 0..n {
        kids[start[d] as usize..start[d + 1] as usize].sort_unstable_by(|a, b| idx.dir_name(*a, true).cmp(idx.dir_name(*b, true)));
    }

    let mut rank = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut roots: Vec<&crate::index::Root> = idx.roots.iter().collect();
    roots.sort_by(|a, b| a.lower.cmp(&b.lower));
    let mut stack: Vec<u32> = roots.iter().rev().map(|r| r.dir).collect();
    while let Some(d) = stack.pop() {
        rank[d as usize] = next;
        next += 1;
        stack.extend(kids[start[d as usize] as usize..start[d as usize + 1] as usize].iter().rev());
    }
    rank
}

fn sort_hits(idx: &Index, hits: &mut [u32], sort: Sort) {
    let ranks = if sort.key == SortKey::Folder { folder_ranks(idx) } else { Vec::new() };
    let cmp = |a: &u32, b: &u32| -> CmpOrdering {
        let (ea, eb) = (&idx.entries[*a as usize], &idx.entries[*b as usize]);
        let by_name = || idx.name_lower(ea).cmp(idx.name_lower(eb));
        let ord = match sort.key {
            SortKey::Name => by_name(),
            SortKey::Folder => ranks[ea.parent as usize].cmp(&ranks[eb.parent as usize]).then_with(by_name),
            SortKey::Size => ea.size.cmp(&eb.size).then_with(by_name),
            SortKey::Modified => ea.modified.cmp(&eb.modified).then_with(by_name),
        };
        if sort.ascending { ord } else { ord.reverse() }
    };
    hits.par_sort_unstable_by(cmp);
}

// ---------------------------------------------------------------------------
// Worker

pub struct Request {
    pub id: u64,
    pub filters: Filters,
    pub sort: Sort,
}

pub struct Outcome {
    pub id: u64,
    pub index_version: u64,
    pub hits: Vec<u32>,
    pub elapsed: Duration,
    pub error: Option<String>,
}

pub struct Searcher {
    tx: crossbeam_channel::Sender<Request>,
    latest: Arc<AtomicU64>,
    pub outcome: Arc<Mutex<Option<Outcome>>>,
}

impl Searcher {
    pub fn new(shared: Arc<Shared>, notify: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Request>();
        let latest = Arc::new(AtomicU64::new(0));
        let outcome = Arc::new(Mutex::new(None));
        let (latest2, outcome2) = (latest.clone(), outcome.clone());
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                while let Ok(mut req) = rx.recv() {
                    // Only the newest query matters.
                    while let Ok(newer) = rx.try_recv() {
                        req = newer;
                    }
                    if let Some(out) = run(&shared, &req, &latest2) {
                        *outcome2.lock() = Some(out);
                        notify();
                    }
                }
            })
            .expect("spawn search worker");
        Self { tx, latest, outcome }
    }

    pub fn submit(&self, req: Request) {
        self.latest.store(req.id, Ordering::SeqCst);
        let _ = self.tx.send(req);
    }
}

fn run(shared: &Shared, req: &Request, latest: &AtomicU64) -> Option<Outcome> {
    let start = Instant::now();
    let idx = shared.index.read();
    let index_version = shared.version.load(Ordering::SeqCst);
    let compiled = match compile(&req.filters, &idx) {
        Ok(c) => c,
        Err(error) => {
            return Some(Outcome { id: req.id, index_version, hits: Vec::new(), elapsed: start.elapsed(), error: Some(error) });
        }
    };
    let id = req.id;
    let mut hits: Vec<u32> = (0..idx.entries.len() as u32)
        .into_par_iter()
        .with_min_len(4096)
        .map_init(String::new, |buf, i| {
            if latest.load(Ordering::Relaxed) != id {
                return None;
            }
            compiled.matches(&idx, &idx.entries[i as usize], buf).then_some(i)
        })
        .flatten()
        .collect();
    if latest.load(Ordering::Relaxed) != id {
        return None;
    }
    sort_hits(&idx, &mut hits, req.sort);
    Some(Outcome { id, index_version, hits, elapsed: start.elapsed(), error: None })
}
