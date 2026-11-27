use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use aho_corasick::AhoCorasickBuilder;
use directories::ProjectDirs;
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use rusqlite::Connection;

const UNKNOWN_DATE: i64 = i64::MIN;
// Distinguish a missing cache entry from a cached "metadata unavailable" result.
const UNCACHED_DATE: i64 = i64::MIN + 1;
const DATE_BATCH_SIZE: usize = 2048;
pub const JOURNAL_NAME: &str = "changes-v3.log";
const DATE_MAGIC: &[u8; 8] = b"MIAODATE";
const MAGIC: &[u8; 8] = b"MIAOIDX3";
static JOURNAL_LOCK: Mutex<()> = Mutex::new(());
const NO_PARENT: u32 = u32::MAX;
const ALIVE: u8 = 1;
const DIRECTORY: u8 = 2;
// Directory metadata updated after launch takes precedence over historical dates.
const UPDATED: u8 = 4;

#[derive(Clone, Copy, Debug)]
struct Record {
    parent: u32,
    name_start: u32,
    name_len: u16,
    flags: u8,
    modified: i64,
}

// Share immutable filename bytes with query workers. Appends copy at most one small
// tail chunk, so typing never copies the whole index or all matching filenames.
const NAME_CHUNK: usize = 4 * 1024 * 1024;
#[derive(Debug)]
enum BaseNames {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}
impl Default for BaseNames {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}
impl std::ops::Deref for BaseNames {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(names) => names,
            Self::Mapped(names) => names,
        }
    }
}
#[derive(Clone, Debug, Default)]
struct NameStore {
    base: Arc<BaseNames>,
    tails: Vec<Arc<Vec<u8>>>,
}
impl From<Vec<u8>> for NameStore {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            base: Arc::new(BaseNames::Owned(bytes)),
            tails: Vec::new(),
        }
    }
}
impl NameStore {
    fn with_capacity(size: usize) -> Self {
        Vec::with_capacity(size).into()
    }
    fn len(&self) -> usize {
        self.base.len()
            + self
                .tails
                .last()
                .map_or(0, |last| (self.tails.len() - 1) * NAME_CHUNK + last.len())
    }
    fn prepare_name(&mut self, len: usize) {
        if self.tails.is_empty()
            && Arc::strong_count(&self.base) == 1
            && matches!(*self.base, BaseNames::Owned(_))
        {
            return;
        }
        if self
            .tails
            .last()
            .is_some_and(|last| last.len() + len > NAME_CHUNK)
        {
            Arc::make_mut(self.tails.last_mut().unwrap()).resize(NAME_CHUNK, 0);
            self.tails.push(Arc::new(Vec::with_capacity(NAME_CHUNK)));
        } else if self.tails.is_empty() {
            self.tails.push(Arc::new(Vec::with_capacity(NAME_CHUNK)));
        }
    }
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if let Some(last) = self.tails.last_mut() {
            Arc::make_mut(last).extend_from_slice(bytes);
        } else {
            let Some(BaseNames::Owned(base)) = Arc::get_mut(&mut self.base) else {
                unreachable!("prepare_name selects a writable tail for shared or mapped names")
            };
            base.extend_from_slice(bytes);
        }
    }
    fn slice(&self, offset: usize, len: usize) -> &[u8] {
        if offset < self.base.len() {
            return &self.base[offset..offset + len];
        }
        let offset = offset - self.base.len();
        &self.tails[offset / NAME_CHUNK][offset % NAME_CHUNK..offset % NAME_CHUNK + len]
    }
    fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.base)?;
        for tail in &self.tails {
            writer.write_all(tail)?;
        }
        Ok(())
    }
}
impl std::ops::Index<std::ops::Range<usize>> for NameStore {
    type Output = [u8];
    fn index(&self, range: std::ops::Range<usize>) -> &[u8] {
        self.slice(range.start, range.end - range.start)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompactIndex {
    records: Vec<Record>,
    names: NameStore,
    path_hashes: Vec<u64>,
    directory_ids: Arc<HashMap<u64, u32>>,
    roots: Vec<PathBuf>,
    active_count: usize,
    dates_ready: bool,
}

#[derive(Clone, Debug)]
struct FlatEntry {
    path: Box<str>,
    is_dir: bool,
    modified: i64,
}

impl CompactIndex {
    pub fn len(&self) -> usize {
        self.active_count
    }
    pub fn is_empty(&self) -> bool {
        self.active_count == 0
    }
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
    pub fn is_dir(&self, index: usize) -> bool {
        self.records[index].flags & DIRECTORY != 0
    }
    fn is_alive(&self, index: usize) -> bool {
        self.records[index].flags & ALIVE != 0
    }
    pub fn name(&self, index: usize) -> &str {
        let record = self.records[index];
        let start = record.name_start as usize;
        let end = start + record.name_len as usize;
        // All inserted names are validated UTF-8.
        unsafe { std::str::from_utf8_unchecked(&self.names[start..end]) }
    }
    pub fn path(&self, mut index: usize) -> String {
        let mut parts = Vec::with_capacity(12);
        loop {
            let record = self.records[index];
            parts.push(self.name(index));
            if record.parent == NO_PARENT {
                break;
            }
            index = record.parent as usize;
        }
        let root = parts.pop().unwrap_or_default();
        let mut result = String::with_capacity(
            root.len() + parts.iter().map(|part| part.len() + 1).sum::<usize>(),
        );
        result.push_str(root);
        while let Some(part) = parts.pop() {
            if !result.ends_with('/') {
                result.push('/');
            }
            result.push_str(part);
        }
        result
    }
    /// Cached membership: parent records precede children. No per-query filesystem I/O.
    pub fn personal_mask(&self, roots: &[PathBuf]) -> Vec<bool> {
        self.personal_mask_with_exclusions(roots, &default_personal_exclusions())
    }
    pub fn personal_mask_with_exclusions(
        &self,
        roots: &[PathBuf],
        exclusions: &[String],
    ) -> Vec<bool> {
        let mut mask = Vec::with_capacity(self.records.len());
        self.extend_personal_mask(&mut mask, roots, exclusions);
        mask
    }

    pub fn extend_personal_mask(
        &self,
        mask: &mut Vec<bool>,
        roots: &[PathBuf],
        exclusions: &[String],
    ) {
        let roots: HashSet<u64> = roots
            .iter()
            .map(|p| hash_path(&p.to_string_lossy()))
            .collect();
        let excluded_names: HashSet<&str> = exclusions.iter().map(String::as_str).collect();
        let hide_bundle_contents = excluded_names.contains("*.app");
        for (id, record) in self.records.iter().enumerate().skip(mask.len()) {
            let name = self.name(id);
            let parent = record.parent as usize;
            let excluded = self.is_dir(id)
                && (excluded_names.contains(name)
                    || (excluded_names.contains(".*") && name.starts_with('.')));
            mask.push(
                roots.contains(&self.path_hashes[id])
                    || (!excluded
                        && record.parent != NO_PARENT
                        && mask[parent]
                        && !(hide_bundle_contents
                            && self.name(parent).ends_with(".app")
                            && !roots.contains(&self.path_hashes[parent]))),
            );
        }
    }

    fn from_flat(roots: Vec<PathBuf>, flat: Vec<FlatEntry>) -> Result<Self, String> {
        let mut index = Self {
            records: Vec::with_capacity(flat.len()),
            names: NameStore::with_capacity(flat.len().saturating_mul(20)),
            path_hashes: Vec::with_capacity(flat.len()),
            directory_ids: Default::default(),
            roots: normalize_roots(roots),
            active_count: 0,
            dates_ready: true,
        };
        index.append_flat(flat)?;
        Ok(index)
    }
    fn append_flat(&mut self, mut flat: Vec<FlatEntry>) -> Result<(), String> {
        flat.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        flat.dedup_by(|a, b| a.path == b.path);
        let directory_ids = Arc::make_mut(&mut self.directory_ids);
        let roots: HashSet<String> = self
            .roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        for entry in flat {
            if self.records.len() >= u32::MAX as usize {
                return Err("索引项目数量超过格式上限".to_owned());
            }
            let path = entry.path.as_ref();
            let (parent, name) = if roots.contains(path) {
                (NO_PARENT, path)
            } else {
                let value = Path::new(path);
                let parent_path = value
                    .parent()
                    .and_then(Path::to_str)
                    .ok_or_else(|| format!("无法确定父目录：{path}"))?;
                let Some(&parent) = directory_ids.get(&hash_path(parent_path)) else {
                    return Err(format!("增量索引缺少父目录：{parent_path}"));
                };
                let name = value
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| format!("无法读取文件名：{path}"))?;
                (parent, name)
            };
            if name.len() > u16::MAX as usize {
                return Err("文件名超过格式上限".to_owned());
            }
            self.names.prepare_name(name.len());
            if self.names.len().saturating_add(name.len()) > u32::MAX as usize {
                return Err("索引文字数据超过格式上限".to_owned());
            }
            let id = self.records.len() as u32;
            let flags = ALIVE | if entry.is_dir { DIRECTORY } else { 0 };
            self.records.push(Record {
                parent,
                name_start: self.names.len() as u32,
                name_len: name.len() as u16,
                flags,
                modified: entry.modified,
            });
            self.names.extend_from_slice(name.as_bytes());
            let hash = hash_path(path);
            self.path_hashes.push(hash);
            self.active_count += 1;
            if entry.is_dir {
                directory_ids.insert(hash, id);
            }
        }
        Ok(())
    }
    pub fn apply_delta(&mut self, delta: IndexDelta) -> Result<(), String> {
        if delta.changed() == 0 {
            return Ok(());
        }
        let removed: HashSet<_> = delta
            .removed_prefixes
            .iter()
            .map(|p| hash_path(p))
            .collect();
        let mut additions: HashMap<_, _> = delta
            .additions
            .into_iter()
            .map(|entry| (hash_path(&entry.path), entry))
            .collect();
        // Keep IDs of directories that still exist. This also preserves the cached
        // full-disk contents of noisy subtrees deliberately pruned from this walk.
        let mut refreshing = vec![false; self.records.len()];
        let directory_ids = Arc::make_mut(&mut self.directory_ids);
        for (id, record) in self.records.iter_mut().enumerate() {
            if record.flags & ALIVE == 0 {
                continue;
            }
            let hash = self.path_hashes[id];
            let covered = removed.contains(&hash)
                || (record.parent != NO_PARENT && refreshing[record.parent as usize]);
            if !covered {
                continue;
            }
            if delta.opaque.contains(&hash) {
                additions.remove(&hash);
                continue;
            }
            let was_directory = record.flags & DIRECTORY != 0;
            refreshing[id] = was_directory;
            if additions
                .get(&hash)
                .is_some_and(|entry| entry.is_dir == was_directory)
            {
                let entry = additions.remove(&hash).unwrap();
                record.modified = entry.modified;
                record.flags |= UPDATED;
                continue;
            }
            record.flags &= !ALIVE;
            self.active_count -= 1;
            if was_directory {
                directory_ids.remove(&hash);
            }
        }
        self.append_flat(additions.into_values().collect())
    }

    pub fn delta_scope(&self) -> DeltaScope {
        DeltaScope {
            roots: self.roots.clone(),
            directory_ids: self.directory_ids.clone(),
        }
    }

    pub fn persist(&mut self) -> Result<(), String> {
        if self.active_count != self.records.len() {
            let mut remap = vec![NO_PARENT; self.records.len()];
            let mut records = Vec::with_capacity(self.active_count);
            let mut names = Vec::with_capacity(self.names.len());
            let mut path_hashes = Vec::with_capacity(self.active_count);
            for old_id in 0..self.records.len() {
                if !self.is_alive(old_id) {
                    continue;
                }
                let old = self.records[old_id];
                let parent = if old.parent == NO_PARENT {
                    NO_PARENT
                } else {
                    remap[old.parent as usize]
                };
                if old.parent != NO_PARENT && parent == NO_PARENT {
                    continue;
                }
                let start = old.name_start as usize;
                let end = start + old.name_len as usize;
                let new_id = records.len() as u32;
                remap[old_id] = new_id;
                records.push(Record {
                    parent,
                    name_start: names.len() as u32,
                    name_len: old.name_len,
                    flags: old.flags,
                    modified: old.modified,
                });
                names.extend_from_slice(&self.names[start..end]);
                path_hashes.push(self.path_hashes[old_id]);
            }
            self.records = records;
            self.names = names.into();
            self.path_hashes = path_hashes;
            self.active_count = self.records.len();
            self.directory_ids = Arc::new(directory_map(&self.records, &self.path_hashes));
        }
        save_compact(self)
    }
}

#[derive(Debug)]
pub enum ScanMessage {
    Progress(usize),
    Finished {
        index: CompactIndex,
        elapsed_ms: u128,
    },
    Failed(String),
}

#[derive(Debug)]
pub struct IndexDelta {
    removed_prefixes: Vec<String>,
    additions: Vec<FlatEntry>,
    changed: usize,
    opaque: HashSet<u64>,
}
impl IndexDelta {
    pub fn changed(&self) -> usize {
        self.changed
    }
}

fn timestamp_ms(value: SystemTime) -> i64 {
    match value.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}
impl CompactIndex {
    pub fn modified(&self, id: usize) -> Option<i64> {
        let date = self.records[id].modified;
        (date != UNKNOWN_DATE && date != UNCACHED_DATE).then_some(date)
    }
    pub fn dates_ready(&self) -> bool {
        self.dates_ready
    }
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
    pub fn apply_historical_dates(
        &mut self,
        original_records: usize,
        dates: &HashMap<u64, Option<i64>>,
    ) {
        if dates.is_empty() {
            return;
        }
        for (&hash, record) in self
            .path_hashes
            .iter()
            .zip(&mut self.records)
            .take(original_records)
        {
            if record.flags & (ALIVE | UPDATED) == ALIVE
                && let Some(date) = dates.get(&hash)
            {
                record.modified = date.unwrap_or(UNKNOWN_DATE);
            }
        }
    }
    pub fn missing_date_batch(&self, cursor: &mut usize) -> Vec<(usize, String)> {
        let mut paths = Vec::new();
        if self.dates_ready {
            return paths;
        }
        while *cursor < self.records.len() && paths.len() < DATE_BATCH_SIZE {
            let id = *cursor;
            *cursor += 1;
            if self.is_alive(id) && self.records[id].modified == UNCACHED_DATE {
                paths.push((id, self.path(id)));
            }
        }
        paths
    }
    pub fn apply_date_batch(&mut self, dates: Vec<(usize, i64)>) {
        // Updates append new records and retire old IDs. A replacement scan
        // discards the pending receiver before any old IDs can be applied.
        for (id, date) in dates {
            if self.is_alive(id) && self.records[id].modified == UNCACHED_DATE {
                self.records[id].modified = date;
            }
        }
    }
    pub fn finish_date_repair(&mut self) -> Result<(), String> {
        self.dates_ready = true;
        self.save_dates()
    }
    pub fn save_dates(&self) -> Result<(), String> {
        if !self.dates_ready {
            return Ok(());
        }
        save_dates_to(self, &data_dir()?.join("modified-v1.bin"))
    }
}
pub fn file_date(path: &Path) -> Option<i64> {
    fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(timestamp_ms)
}
pub fn read_date_batch(paths: Vec<(usize, String)>) -> Vec<(usize, i64)> {
    paths
        .into_iter()
        .map(|(id, path)| {
            let date = file_date(Path::new(&path)).unwrap_or(UNKNOWN_DATE);
            (id, date)
        })
        .collect()
}

fn save_dates_to(index: &CompactIndex, path: &Path) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut writer = BufWriter::with_capacity(
        1024 * 1024,
        File::create(&temporary).map_err(|e| e.to_string())?,
    );
    writer.write_all(DATE_MAGIC).map_err(|e| e.to_string())?;
    write_u64(&mut writer, index.active_count as u64)?;
    for (hash, record) in index.path_hashes.iter().zip(&index.records) {
        if record.flags & ALIVE == 0 {
            continue;
        }
        write_u64(&mut writer, *hash)?;
        writer
            .write_all(&record.modified.to_le_bytes())
            .map_err(|e| e.to_string())?;
    }
    writer
        .flush()
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())
}
fn load_dates(index: &mut CompactIndex, path: &Path) -> Result<(), String> {
    let mut reader =
        BufReader::with_capacity(1024 * 1024, File::open(path).map_err(|e| e.to_string())?);
    let mut magic = [0; 8];
    reader.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != DATE_MAGIC {
        return Err("日期缓存格式错误".into());
    }
    let count = read_u64(&mut reader)?;
    if count > u32::MAX as u64
        || fs::metadata(path).map_err(|e| e.to_string())?.len() != 16 + count * 16
    {
        return Err("日期缓存损坏".into());
    }
    // The writer emits the same order as the index. Stream directly into
    // records on a normal launch, checking every path before accepting its date.
    if count == index.active_count as u64 {
        let mut aligned = true;
        let mut complete = true;
        for (&expected, record) in index.path_hashes.iter().zip(&mut index.records) {
            if record.flags & ALIVE == 0 {
                continue;
            }
            if read_u64(&mut reader)? != expected {
                aligned = false;
                break;
            }
            record.modified = read_u64(&mut reader)? as i64;
            complete &= record.modified != UNCACHED_DATE;
        }
        if aligned {
            index.dates_ready = complete;
            return Ok(());
        }
        reader
            .seek(SeekFrom::Start(16))
            .map_err(|error| error.to_string())?;
    }
    let mut dates = HashMap::with_capacity(count as usize);
    for _ in 0..count {
        let hash = read_u64(&mut reader)?;
        let date = read_u64(&mut reader)? as i64;
        dates.insert(hash, date);
    }
    index.dates_ready = index
        .path_hashes
        .iter()
        .zip(&index.records)
        .filter(|(_, r)| r.flags & ALIVE != 0)
        .all(|(hash, _)| dates.get(hash).is_some_and(|date| *date != UNCACHED_DATE));
    for (hash, record) in index.path_hashes.iter().zip(&mut index.records) {
        record.modified = dates.get(hash).copied().unwrap_or(UNCACHED_DATE);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SortOrder {
    #[default]
    Relevance,
    Latest,
    Oldest,
    Name,
}
impl SortOrder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "相关性优先",
            Self::Latest => "最新修改优先",
            Self::Oldest => "最早修改优先",
            Self::Name => "名称升序",
        }
    }
}
pub fn search_sorted(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
) -> Vec<usize> {
    search_sorted_with_scope(index, query, limit, category, order, true)
}

pub fn search_sorted_with_scope(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
    include_paths: bool,
) -> Vec<usize> {
    if matches!(order, SortOrder::Latest | SortOrder::Oldest) && !index.dates_ready {
        return search_filtered_with_scope(index, query, limit, category, include_paths);
    }
    if limit == 0 {
        return Vec::new();
    }
    let results = search_filtered_with_scope(index, query, usize::MAX, category, include_paths);
    sort_hits(index, query, limit, category, order, results)
}

/// Apply scope/date predicates to all matches BEFORE selecting the global top results.
pub fn search_with_filters(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
    include_paths: bool,
    personal: Option<&[bool]>,
    since: Option<i64>,
) -> Vec<usize> {
    search_with_options(
        index,
        query,
        limit,
        category,
        order,
        include_paths,
        personal,
        since,
        false,
    )
}
pub fn default_personal_exclusions() -> Vec<String> {
    [
        ".*",
        "*.app",
        "Library",
        "Caches",
        "node_modules",
        "target",
        "venv",
        "__pycache__",
        "dist",
        "build",
        "app-content",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
pub fn previewable_image(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ["png", "jpg", "jpeg", "webp", "gif"]
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
    })
}
pub fn search_with_options(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
    include_paths: bool,
    personal: Option<&[bool]>,
    since: Option<i64>,
    raster: bool,
) -> Vec<usize> {
    search_with_date_range(
        index,
        query,
        limit,
        category,
        order,
        include_paths,
        personal,
        since,
        None,
        raster,
    )
}
/// End is exclusive; compare cached millisecond timestamps without filesystem I/O.
pub fn search_with_date_range(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
    include_paths: bool,
    personal: Option<&[bool]>,
    since: Option<i64>,
    until: Option<i64>,
    raster: bool,
) -> Vec<usize> {
    if personal.is_none() && since.is_none() && until.is_none() && !raster && !query.contains('/') {
        return search_sorted_with_scope(index, query, limit, category, order, include_paths);
    }
    if limit == 0 {
        return Vec::new();
    }
    let (name_query, hits) = matching_hits(
        index,
        query,
        category,
        include_paths,
        personal,
        since,
        until,
        raster,
    );
    sort_hits(index, &name_query, limit, category, order, hits)
}

fn matching_hits(
    index: &CompactIndex,
    query: &str,
    category: Category,
    include_paths: bool,
    personal: Option<&[bool]>,
    since: Option<i64>,
    until: Option<i64>,
    raster: bool,
) -> (String, Vec<usize>) {
    // Empty startup stays empty; an explicit type/scope/date filter can browse files.
    if query.trim().is_empty()
        && category == Category::All
        && personal.is_none()
        && since.is_none()
        && until.is_none()
        && !raster
    {
        return (String::new(), Vec::new());
    }
    let (path_terms, name_terms): (Vec<_>, Vec<_>) =
        query.split_whitespace().partition(|t| t.contains('/'));
    let name_query = name_terms.join(" ");
    let mut hits = if name_query.is_empty() {
        index
            .records
            .par_iter()
            .enumerate()
            .filter_map(|(id, _)| {
                (index.is_alive(id)
                    && (category == Category::All || index.category(id) == category)
                    && personal.is_none_or(|m| m.get(id).copied().unwrap_or(false))
                    && date_matches(index.modified(id), since, until))
                .then_some(id)
            })
            .collect::<Vec<_>>()
    } else {
        search_filtered_with_scope(index, &name_query, usize::MAX, category, include_paths)
    };
    if !path_terms.is_empty() {
        let path_query = path_terms
            .iter()
            .map(|t| t.trim_matches('/'))
            .collect::<Vec<_>>()
            .join(" ");
        let path_hits =
            search_filtered_with_scope(index, &path_query, usize::MAX, Category::All, true);
        let mut membership = vec![false; index.records.len()];
        for id in path_hits {
            membership[id] = true;
        }
        hits.retain(|id| membership[*id]);
    }
    hits.retain(|id| {
        (!raster || (index.category(*id) == Category::Image && previewable_image(index.name(*id))))
            && personal.is_none_or(|m| m.get(*id).copied().unwrap_or(false))
            && date_matches(index.modified(*id), since, until)
    });
    (name_query, hits)
}

fn date_matches(value: Option<i64>, since: Option<i64>, until: Option<i64>) -> bool {
    if since.is_none() && until.is_none() {
        return true;
    }
    value.is_some_and(|d| since.is_none_or(|s| d >= s) && until.is_none_or(|e| d < e))
}
#[cfg(test)]
#[test]
fn date_range_boundaries_and_unknown_dates() {
    assert!(date_matches(Some(1000), Some(1000), Some(2000)));
    assert!(date_matches(Some(1999), Some(1000), Some(2000)));
    assert!(!date_matches(Some(2000), Some(1000), Some(2000)));
    assert!(!date_matches(Some(999), Some(1000), Some(2000)));
    assert!(!date_matches(None, Some(1000), Some(2000)));
    assert!(date_matches(None, None, None));
}

fn sort_hits(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    order: SortOrder,
    mut results: Vec<usize>,
) -> Vec<usize> {
    if order == SortOrder::Relevance {
        let query: Vec<u8> = query
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| b.to_ascii_lowercase())
            .collect();
        // Cache ranking once per hit; comparisons never rebuild full paths.
        let mut ranked: Vec<_> = results
            .into_iter()
            .map(|id| {
                let (app_tier, quality) = rank_key(index, id, &query, category);
                (id, app_tier, quality)
            })
            .collect();
        let compare = |a: &(usize, u8, u8), b: &(usize, u8, u8)| {
            (a.1, a.2)
                .cmp(&(b.1, b.2))
                .then_with(|| index.name(a.0).cmp(index.name(b.0)))
                .then_with(|| a.0.cmp(&b.0))
        };
        if ranked.len() > limit {
            ranked.select_nth_unstable_by(limit, compare);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(compare);
        return ranked.into_iter().map(|r| r.0).collect();
    }
    let compare = |a: &usize, b: &usize| {
        if order == SortOrder::Name {
            return index.name(*a).cmp(index.name(*b)).then_with(|| a.cmp(b));
        }
        match (index.modified(*a), index.modified(*b)) {
            (Some(a_date), Some(b_date)) => {
                let cmp = if order == SortOrder::Latest {
                    b_date.cmp(&a_date)
                } else {
                    a_date.cmp(&b_date)
                };
                cmp.then_with(|| a.cmp(b))
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    };
    if results.len() > limit {
        results.select_nth_unstable_by(limit, compare);
        results.truncate(limit);
    }
    results.sort_unstable_by(compare);
    results
}

fn rank_key(index: &CompactIndex, id: usize, query: &[u8], category: Category) -> (u8, u8) {
    let name = index.name(id);
    let stem = name.strip_suffix(".app").unwrap_or(name);
    let normalized = || {
        stem.bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .map(|b| b.to_ascii_lowercase())
    };
    let quality = if normalized().eq(query.iter().copied()) {
        0
    } else if normalized().take(query.len()).eq(query.iter().copied()) {
        1
    } else {
        2
    };
    let app_tier = if category == Category::Application && name.ends_with(".app") {
        let path = index.path(id);
        if path.contains(".app/") {
            3
        } else if path.starts_with("/private/var/") || path.starts_with("/tmp/") {
            4
        } else if path.starts_with("/Applications/") || path.contains("/Applications/") {
            0
        } else {
            1
        }
    } else {
        0
    };
    (app_tier, quality)
}

/// A query snapshot contains only matching IDs and their sorting keys, never the full index.
/// Each page partitions the cached matches and sorts just its requested window.
pub struct SearchSession {
    keys: PageKeys,
    order: SortOrder,
}
struct NameKey {
    id: u32,
    offset: u32,
    len: u16,
    tier: u8,
    quality: u8,
}
enum PageKeys {
    Names {
        keys: Vec<NameKey>,
        names: NameStore,
    },
    Dates(Vec<(u32, i64)>),
}
impl SearchSession {
    pub fn new(
        index: &CompactIndex,
        query: &str,
        category: Category,
        order: SortOrder,
        include_paths: bool,
        personal: Option<&[bool]>,
        since: Option<i64>,
        until: Option<i64>,
        raster: bool,
    ) -> Self {
        let (name_query, hits) = matching_hits(
            index,
            query,
            category,
            include_paths,
            personal,
            since,
            until,
            raster,
        );
        let keys = if matches!(order, SortOrder::Latest | SortOrder::Oldest) {
            PageKeys::Dates(
                hits.into_iter()
                    .map(|id| (id as u32, index.modified(id).unwrap_or(UNKNOWN_DATE)))
                    .collect(),
            )
        } else {
            let query: Vec<u8> = name_query
                .bytes()
                .filter(|b| !b.is_ascii_whitespace())
                .map(|b| b.to_ascii_lowercase())
                .collect();
            let keys = hits
                .into_iter()
                .map(|id| {
                    let record = index.records[id];
                    let (tier, quality) = if order == SortOrder::Relevance {
                        rank_key(index, id, &query, category)
                    } else {
                        (0, 0)
                    };
                    let key = NameKey {
                        id: id as u32,
                        offset: record.name_start,
                        len: record.name_len,
                        tier,
                        quality,
                    };
                    key
                })
                .collect();
            PageKeys::Names {
                keys,
                names: index.names.clone(),
            }
        };
        Self { keys, order }
    }
    pub fn total(&self) -> usize {
        match &self.keys {
            PageKeys::Names { keys, .. } => keys.len(),
            PageKeys::Dates(keys) => keys.len(),
        }
    }
    pub fn page(&mut self, page: usize, size: usize) -> Vec<usize> {
        let start = page.saturating_mul(size);
        if size == 0 || start >= self.total() {
            return Vec::new();
        }
        match &mut self.keys {
            PageKeys::Names { keys, names } => {
                let compare = |a: &NameKey, b: &NameKey| {
                    (a.tier, a.quality)
                        .cmp(&(b.tier, b.quality))
                        .then_with(|| {
                            let name =
                                |key: &NameKey| names.slice(key.offset as usize, key.len as usize);
                            name(a).cmp(name(b))
                        })
                        .then_with(|| a.id.cmp(&b.id))
                };
                window(keys, start, size, compare)
                    .iter()
                    .map(|k| k.id as usize)
                    .collect()
            }
            PageKeys::Dates(keys) => {
                let order = self.order;
                let compare = |a: &(u32, i64), b: &(u32, i64)| {
                    (a.1 == UNKNOWN_DATE)
                        .cmp(&(b.1 == UNKNOWN_DATE))
                        .then_with(|| {
                            if order == SortOrder::Latest {
                                b.1.cmp(&a.1)
                            } else {
                                a.1.cmp(&b.1)
                            }
                        })
                        .then_with(|| a.0.cmp(&b.0))
                };
                window(keys, start, size, compare)
                    .iter()
                    .map(|k| k.0 as usize)
                    .collect()
            }
        }
    }
}
fn window<T, F: Fn(&T, &T) -> std::cmp::Ordering + Copy>(
    keys: &mut [T],
    start: usize,
    size: usize,
    compare: F,
) -> &mut [T] {
    if start > 0 {
        keys.select_nth_unstable_by(start, compare);
    }
    let tail = &mut keys[start..];
    let len = size.min(tail.len());
    if len < tail.len() {
        tail.select_nth_unstable_by(len, compare);
    }
    tail[..len].sort_unstable_by(compare);
    &mut tail[..len]
}

pub fn warm_up_parallel_search() {
    let _ = rayon::ThreadPoolBuilder::new().build_global();
}

pub fn full_disk_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [
        "/Applications",
        "/Library",
        "/System",
        "/Users",
        "/opt",
        "/private",
        "/usr",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    roots.extend(local_external_volumes());
    normalize_roots(roots)
}

#[cfg(target_os = "macos")]
pub fn local_external_volumes() -> Vec<PathBuf> {
    // Preferences and index setup can enumerate mounts concurrently. Ask the
    // kernel for a private snapshot instead of sharing getmntinfo's static array.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    let mut mounts: Vec<libc::statfs> = Vec::with_capacity(count as usize);
    let Ok(size) = i32::try_from(mounts.capacity() * size_of::<libc::statfs>()) else {
        return Vec::new();
    };
    let count = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    // SAFETY: getfsstat initialized the entries that fit in the supplied buffer.
    unsafe {
        mounts.set_len((count as usize).min(mounts.capacity()));
    }
    let mut volumes = Vec::new();
    for mount in mounts {
        if mount.f_flags & libc::MNT_LOCAL as u32 == 0 {
            continue;
        }
        // SAFETY: macOS supplies a null-terminated mount path.
        let raw = unsafe { CStr::from_ptr(mount.f_mntonname.as_ptr()) };
        let path = PathBuf::from(raw.to_string_lossy().as_ref());
        if path.starts_with("/Volumes/") {
            volumes.push(path);
        }
    }
    volumes
}
#[cfg(not(target_os = "macos"))]
pub fn local_external_volumes() -> Vec<PathBuf> {
    Vec::new()
}

fn data_dir() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "gkgy", "MiaoFind")
        .ok_or_else(|| "无法确定应用数据目录".to_owned())?;
    fs::create_dir_all(dirs.data_dir()).map_err(|error| error.to_string())?;
    Ok(dirs.data_dir().to_path_buf())
}
pub fn compact_index_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("index-v3.bin"))
}
fn journal_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join(JOURNAL_NAME))
}
fn legacy_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("index.sqlite3"))
}

pub fn load() -> Result<CompactIndex, String> {
    let mut index = load_cached()?;
    index.apply_delta(pending_journal_delta(index.delta_scope())?)?;
    Ok(index)
}

pub fn pending_journal_delta(scope: DeltaScope) -> Result<IndexDelta, String> {
    scope.collect_delta(read_journal(&journal_path()?, &scope.roots)?)
}

pub fn load_cached() -> Result<CompactIndex, String> {
    let path = compact_index_path()?;
    let mut index = if path.exists() {
        load_compact(&path)?
    } else if legacy_path()?.exists() {
        migrate_legacy()?
    } else {
        CompactIndex::default()
    };
    let _ = load_dates(&mut index, &data_dir()?.join("modified-v1.bin"));
    Ok(index)
}

fn migrate_legacy() -> Result<CompactIndex, String> {
    let connection = Connection::open(legacy_path()?).map_err(|error| error.to_string())?;
    let mut roots = Vec::new();
    {
        let mut statement = connection
            .prepare("SELECT path FROM roots ORDER BY path")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        for row in rows {
            roots.push(PathBuf::from(row.map_err(|e| e.to_string())?));
        }
    }
    let mut statement = connection
        .prepare("SELECT path, is_dir FROM entries ORDER BY path")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(FlatEntry {
                path: row.get::<_, String>(0)?.into_boxed_str(),
                is_dir: row.get::<_, i64>(1)? != 0,
                modified: UNCACHED_DATE,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut flat = Vec::new();
    for row in rows {
        flat.push(row.map_err(|error| error.to_string())?);
    }
    // An older SQLite index can include entries on a currently unmounted volume.
    let roots = normalize_roots(roots);
    flat.retain(|entry| {
        roots
            .iter()
            .any(|root| Path::new(entry.path.as_ref()).starts_with(root))
    });
    let mut index = CompactIndex::from_flat(roots, flat)?;
    index.dates_ready = false;
    let _ = load_dates(&mut index, &data_dir()?.join("modified-v1.bin"));
    save_compact(&index)?;
    Ok(index)
}

fn save_compact(index: &CompactIndex) -> Result<(), String> {
    write_snapshot(
        index,
        &compact_index_path()?,
        &data_dir()?.join("modified-v1.bin"),
    )
}

fn write_snapshot(index: &CompactIndex, path: &Path, dates: &Path) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut writer = BufWriter::with_capacity(
        1024 * 1024,
        File::create(&temporary).map_err(|error| error.to_string())?,
    );
    writer.write_all(MAGIC).map_err(|error| error.to_string())?;
    write_u32(&mut writer, index.roots.len() as u32)?;
    for root in &index.roots {
        let root = root.to_string_lossy();
        write_u32(&mut writer, root.len() as u32)?;
        writer
            .write_all(root.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    write_u64(&mut writer, index.records.len() as u64)?;
    write_u64(&mut writer, index.names.len() as u64)?;
    for record in &index.records {
        writer
            .write_all(&record.parent.to_le_bytes())
            .and_then(|_| writer.write_all(&record.name_start.to_le_bytes()))
            .and_then(|_| writer.write_all(&record.name_len.to_le_bytes()))
            .and_then(|_| writer.write_all(&[record.flags & (ALIVE | DIRECTORY), 0]))
            .map_err(|error| error.to_string())?;
    }
    index
        .names
        .write_to(&mut writer)
        .map_err(|error| error.to_string())?;
    writer
        .flush()
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    // A checkpoint may run during date repair. Persist the per-record missing
    // sentinel too, so flushed live dates survive and repair resumes on restart.
    save_dates_to(index, dates)?;
    if let Some(dir) = path.parent() {
        File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Read a frozen index and its date cache without walking the filesystem or replaying changes.
pub fn load_compact_with_dates(path: &Path, dates: &Path) -> Result<CompactIndex, String> {
    let mut index = load_compact(path)?;
    load_dates(&mut index, dates)?;
    Ok(index)
}

pub fn load_compact(path: &Path) -> Result<CompactIndex, String> {
    let mut reader = BufReader::with_capacity(
        1024 * 1024,
        File::open(path).map_err(|error| error.to_string())?,
    );
    let mut magic = [0u8; 8];
    reader
        .read_exact(&mut magic)
        .map_err(|error| error.to_string())?;
    if &magic != MAGIC {
        return Err("索引格式无法识别，需要重新建立索引".to_owned());
    }
    let root_count = read_u32(&mut reader)? as usize;
    let mut roots = Vec::with_capacity(root_count);
    for _ in 0..root_count {
        let length = read_u32(&mut reader)? as usize;
        let mut bytes = vec![0; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| error.to_string())?;
        roots.push(PathBuf::from(
            String::from_utf8(bytes).map_err(|e| e.to_string())?,
        ));
    }
    let count = read_u64(&mut reader)? as usize;
    let names_len = read_u64(&mut reader)? as usize;
    if count > u32::MAX as usize || names_len > u32::MAX as usize {
        return Err("索引数据超过当前版本支持范围".to_owned());
    }
    // Leave modest headroom for incremental updates. The first filesystem
    // event should not force a reallocation of the full loaded tables.
    let capacity = count + (count / 128).min(32_768);
    let mut records = Vec::with_capacity(capacity);
    for _ in 0..count {
        let parent = read_u32(&mut reader)?;
        let name_start = read_u32(&mut reader)?;
        let name_len = read_u16(&mut reader)?;
        let mut flags = [0; 2];
        reader
            .read_exact(&mut flags)
            .map_err(|error| error.to_string())?;
        records.push(Record {
            parent,
            name_start,
            name_len,
            flags: flags[0] & (ALIVE | DIRECTORY),
            modified: UNCACHED_DATE,
        });
    }
    let names_offset = reader.stream_position().map_err(|e| e.to_string())?;
    let file_len = reader
        .get_ref()
        .metadata()
        .map_err(|e| e.to_string())?
        .len();
    if file_len < names_offset + names_len as u64 {
        return Err("索引名称表不完整".into());
    }
    let names = if names_len == 0 {
        NameStore::default()
    } else {
        // save_compact replaces the index by rename. An active mapping keeps
        // its immutable inode, while newly indexed names go into writable tails.
        let mapped = unsafe {
            memmap2::MmapOptions::new()
                .offset(names_offset)
                .len(names_len)
                .map(reader.get_ref())
        }
        .map_err(|error| error.to_string())?;
        std::str::from_utf8(&mapped).map_err(|e| e.to_string())?;
        NameStore {
            base: Arc::new(BaseNames::Mapped(mapped)),
            tails: Vec::new(),
        }
    };
    let mut path_hashes = Vec::with_capacity(capacity);
    for (index, record) in records.iter().enumerate() {
        let start = record.name_start as usize;
        let end = start + record.name_len as usize;
        if end > names.len() || (record.parent != NO_PARENT && record.parent as usize >= index) {
            return Err("索引记录损坏，需要重新建立索引".to_owned());
        }
        let hash = if record.parent == NO_PARENT {
            hash_bytes(&names[start..end])
        } else {
            let parent = records[record.parent as usize];
            let parent_end = parent.name_start as usize + parent.name_len as usize;
            let has_separator = parent.parent == NO_PARENT
                && parent.name_len > 0
                && names.slice(parent_end - 1, 1) == b"/";
            let mut hash = path_hashes[record.parent as usize];
            if !has_separator {
                hash = extend_hash(hash, b"/");
            }
            extend_hash(hash, &names[start..end])
        };
        path_hashes.push(hash);
    }
    let active_count = records
        .iter()
        .filter(|record| record.flags & ALIVE != 0)
        .count();
    Ok(CompactIndex {
        directory_ids: Arc::new(directory_map(&records, &path_hashes)),
        records,
        names,
        path_hashes,
        roots: normalize_roots(roots),
        active_count,
        dates_ready: false,
    })
}

pub fn scan(roots: Vec<PathBuf>, sender: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        let roots = normalize_roots(roots);
        if roots.is_empty() {
            let _ = sender.send(ScanMessage::Failed("没有可索引的位置".to_owned()));
            return;
        }
        let started = Instant::now();
        let entries = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let mut builder = WalkBuilder::new(&roots[0]);
        for root in roots.iter().skip(1) {
            builder.add(root);
        }
        builder
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(false)
            .filter_entry(|entry| !should_skip(entry.path()))
            .threads(std::thread::available_parallelism().map_or(4, usize::from));
        builder.build_parallel().run(|| {
            let entries = Arc::clone(&entries);
            let count = Arc::clone(&count);
            let sender = sender.clone();
            Box::new(move |item| {
                let Ok(item) = item else {
                    return WalkState::Continue;
                };
                if let Some(entry) =
                    make_flat(item.path(), item.file_type().map(|kind| kind.is_dir()))
                {
                    entries.lock().expect("scan lock poisoned").push(entry);
                    let current = count.fetch_add(1, Ordering::Relaxed) + 1;
                    if current.is_multiple_of(2_000) {
                        let _ = sender.send(ScanMessage::Progress(current));
                    }
                }
                WalkState::Continue
            })
        });
        let flat = Arc::try_unwrap(entries)
            .expect("scan workers active")
            .into_inner()
            .expect("scan lock poisoned");
        if flat.is_empty() {
            let _ = sender.send(ScanMessage::Failed("没有读取到任何文件".to_owned()));
            return;
        }
        let index = match CompactIndex::from_flat(roots, flat) {
            Ok(value) => value,
            Err(error) => {
                let _ = sender.send(ScanMessage::Failed(format!("整理索引失败：{error}")));
                return;
            }
        };
        if let Err(error) = save_compact(&index) {
            let _ = sender.send(ScanMessage::Failed(format!("保存索引失败：{error}")));
            return;
        }
        let _ = sender.send(ScanMessage::Finished {
            index,
            elapsed_ms: started.elapsed().as_millis(),
        });
    });
}

fn make_flat(path: &Path, known_is_dir: Option<bool>) -> Option<FlatEntry> {
    let value = path.to_str()?.to_owned();
    let metadata = fs::symlink_metadata(path).ok();
    let is_dir = known_is_dir.unwrap_or_else(|| metadata.as_ref().is_some_and(|m| m.is_dir()));
    let modified = metadata
        .and_then(|m| m.modified().ok())
        .map(timestamp_ms)
        .unwrap_or(UNKNOWN_DATE);
    Some(FlatEntry {
        path: value.into_boxed_str(),
        is_dir,
        modified,
    })
}

fn directory_map(records: &[Record], hashes: &[u64]) -> HashMap<u64, u32> {
    records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.flags & (ALIVE | DIRECTORY) == ALIVE | DIRECTORY)
        .map(|(id, _)| (hashes[id], id as u32))
        .collect()
}

/// A worker only needs the indexed directory IDs, never a copy of all file records.
pub struct DeltaScope {
    roots: Vec<PathBuf>,
    directory_ids: Arc<HashMap<u64, u32>>,
}

pub fn sync_path(path: &Path, roots: &[PathBuf]) -> bool {
    let Some(root) = roots.iter().find(|root| path.starts_with(root)) else {
        return false;
    };
    if should_skip(path)
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return false;
    }
    // Do not feed our own journal, snapshots or eframe autosaves back to the watcher.
    if path.ancestors().any(|p| {
        p.ends_with("Library/Application Support/com.gkgy.MiaoFind")
            || p.ends_with("Library/Application Support/随手找")
    }) {
        return false;
    }
    // An explicitly indexed project or cache within a temporary directory still works.
    for prefix in [
        "/private/var/folders",
        "/private/tmp",
        "/var/folders",
        "/tmp",
        "/System/Library/AssetsV2",
        "/Library/Application Support/Apple/AssetCache",
        "/private/var/db/biome",
    ] {
        if path.starts_with(prefix) && !root.starts_with(prefix) {
            return false;
        }
    }
    let relative = path.strip_prefix(root).unwrap();
    !relative.components().any(|c| c.as_os_str() == "Caches")
}

/// Path ordering compares components, so a directory and all of its descendants
/// are adjacent. Never invent a common parent for independent sibling events.
pub fn dedup_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_unstable();
    let mut kept = Vec::<PathBuf>::new();
    for path in paths {
        if kept.last().is_none_or(|parent| !path.starts_with(parent)) {
            kept.push(path);
        }
    }
    kept
}

impl DeltaScope {
    fn scan_paths(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut result = Vec::new();
        for path in paths {
            if !sync_path(&path, &self.roots) {
                continue;
            }
            let root = self.roots.iter().find(|r| path.starts_with(r)).unwrap();
            if &path == root
                && self
                    .directory_ids
                    .contains_key(&hash_path(&root.to_string_lossy()))
            {
                // A watched root's metadata event must never trigger a volume-wide walk.
                continue;
            }
            let mut start = path.as_path();
            while start != root {
                let parent = start.parent().unwrap();
                if self
                    .directory_ids
                    .contains_key(&hash_path(&parent.to_string_lossy()))
                {
                    break;
                }
                start = parent;
            }
            result.push(start.to_owned());
        }
        dedup_paths(result)
    }

    pub fn collect_delta(&self, paths: Vec<PathBuf>) -> Result<IndexDelta, String> {
        let paths = self.scan_paths(paths);
        let mut additions = Vec::new();
        let mut opaque = HashSet::new();
        let mut removed_prefixes = Vec::new();
        let mut directories = Vec::new();
        for path in paths {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => directories.push(path.clone()),
                Ok(_) => additions.extend(make_flat(&path, Some(false))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
            removed_prefixes.push(path.to_string_lossy().into_owned());
        }
        if let Some((first, rest)) = directories.split_first() {
            let mut walk = WalkBuilder::new(first);
            for path in rest {
                walk.add(path);
            }
            // Each worker buffers its own entries. Merge once per worker, not once per file.
            let output = Mutex::new((&mut additions, &mut opaque));
            let mut visitor = DeltaVisitors {
                output: &output,
                roots: &self.roots,
            };
            walk.hidden(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .follow_links(false)
                .threads(rayon::current_num_threads().min(8))
                .build_parallel()
                .visit(&mut visitor);
        }
        let changed = additions.len() + removed_prefixes.len();
        Ok(IndexDelta {
            additions,
            removed_prefixes,
            opaque,
            changed,
        })
    }
}

#[derive(Clone, Copy)]
struct DeltaVisitors<'a, 'b> {
    output: &'a Mutex<(&'b mut Vec<FlatEntry>, &'b mut HashSet<u64>)>,
    roots: &'a [PathBuf],
}
struct DeltaVisitor<'a, 'b> {
    shared: DeltaVisitors<'a, 'b>,
    entries: Vec<FlatEntry>,
    opaque: HashSet<u64>,
}
impl<'s> ignore::ParallelVisitorBuilder<'s> for DeltaVisitors<'s, '_> {
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 's> {
        Box::new(DeltaVisitor {
            shared: *self,
            entries: Vec::new(),
            opaque: HashSet::new(),
        })
    }
}
impl ignore::ParallelVisitor for DeltaVisitor<'_, '_> {
    fn visit(&mut self, item: Result<ignore::DirEntry, ignore::Error>) -> WalkState {
        match item {
            Ok(item) => {
                let path = item.path();
                if !sync_path(path, self.shared.roots) {
                    self.opaque.insert(hash_path(&path.to_string_lossy()));
                    return WalkState::Skip;
                }
                self.entries
                    .extend(make_flat(path, item.file_type().map(|kind| kind.is_dir())));
            }
            Err(ignore::Error::WithPath { path, .. }) => {
                self.opaque.insert(hash_path(&path.to_string_lossy()));
            }
            Err(_) => (),
        }
        WalkState::Continue
    }
}
impl Drop for DeltaVisitor<'_, '_> {
    fn drop(&mut self) {
        let mut out = self.shared.output.lock().unwrap();
        out.0.append(&mut self.entries);
        out.1.extend(self.opaque.drain());
    }
}

pub fn checkpoint_journal(index: &CompactIndex, end: u64) -> Result<(), String> {
    let dir = data_dir()?;
    checkpoint_to(
        index,
        end,
        &dir.join("index-v3.bin"),
        &dir.join("modified-v1.bin"),
        &dir.join(JOURNAL_NAME),
    )
}

fn checkpoint_to(
    index: &CompactIndex,
    end: u64,
    snapshot: &Path,
    dates: &Path,
    journal: &Path,
) -> Result<(), String> {
    write_snapshot(index, snapshot, dates)?;
    acknowledge_journal(journal, end)
}

pub fn acknowledge_applied(end: u64) -> Result<(), String> {
    acknowledge_journal(&journal_path()?, end)
}

// A later or unapplied batch always stays replayable. Only the single UI writer
// acknowledges batches, while dispatch is paused for a checkpoint.
fn acknowledge_journal(path: &Path, end: u64) -> Result<(), String> {
    let _guard = JOURNAL_LOCK.lock().unwrap();
    if end > 0 {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() == end {
            file.set_len(0).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub fn journal_length() -> Result<u64, String> {
    match fs::metadata(journal_path()?) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.to_string()),
    }
}

pub fn append_journal(paths: &[PathBuf]) -> Result<(), String> {
    write_journal(&journal_path()?, paths)
}
pub fn write_journal(path: &Path, paths: &[PathBuf]) -> Result<(), String> {
    let _guard = JOURNAL_LOCK.lock().unwrap();
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let start = file.metadata().map_err(|e| e.to_string())?.len();
    let result = (|| {
        let mut writer = BufWriter::new(&file);
        for path in paths {
            let value = path.to_string_lossy();
            write_u32(&mut writer, value.len() as u32)?;
            writer
                .write_all(value.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        writer.flush().map_err(|error| error.to_string())
    })();
    if result.is_err() {
        file.set_len(start).map_err(|e| e.to_string())?;
    }
    result
}

fn read_journal(path: &Path, roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let _guard = JOURNAL_LOCK.lock().unwrap();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut reader = BufReader::new(file);
    let mut complete_bytes = 0u64;
    let mut paths = Vec::new();
    loop {
        let mut length = [0; 4];
        match reader.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > 1024 * 1024 {
            return Err("变更日志路径长度无效".into());
        }
        let mut bytes = vec![0; length];
        match reader.read_exact(&mut bytes) {
            Ok(()) => {
                paths.push(PathBuf::from(
                    String::from_utf8(bytes).map_err(|e| e.to_string())?,
                ));
                complete_bytes += 4 + length as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    if reader
        .get_ref()
        .metadata()
        .map_err(|e| e.to_string())?
        .len()
        != complete_bytes
    {
        OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_len(complete_bytes))
            .map_err(|e| e.to_string())?;
    }
    paths.retain(|p| !roots.contains(p) && sync_path(p, roots));
    Ok(dedup_paths(paths))
}

pub fn normalize_roots(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.retain(|root| root.is_absolute() && root.exists());
    roots.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    let mut result = Vec::<PathBuf>::new();
    for root in roots {
        if !result.iter().any(|parent| root.starts_with(parent)) {
            result.push(root);
        }
    }
    result
}

fn should_skip(path: &Path) -> bool {
    const PREFIXES: &[&str] = &[
        "/System/Volumes/Data",
        "/System/Volumes/Preboot",
        "/System/Volumes/Update",
        "/System/Volumes/VM",
        "/System/Volumes/xarts",
        "/System/Volumes/Hardware",
        "/System/Volumes/iSCPreboot",
        "/System/Volumes/FieldService",
        "/Volumes/.timemachine",
        "/Volumes/Backups.backupdb",
        "/private/var/run",
    ];
    PREFIXES
        .iter()
        .any(|prefix| path == Path::new(prefix) || path.starts_with(prefix))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Category {
    #[default]
    All,
    Folder,
    Document,
    Image,
    Audio,
    Video,
    Application,
    Archive,
    Code,
    Other,
}
impl Category {
    pub const ALL: [Self; 10] = [
        Self::All,
        Self::Folder,
        Self::Document,
        Self::Image,
        Self::Audio,
        Self::Video,
        Self::Application,
        Self::Archive,
        Self::Code,
        Self::Other,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Folder => "文件夹",
            Self::Document => "文档",
            Self::Image => "图片",
            Self::Audio => "音频",
            Self::Video => "视频",
            Self::Application => "应用",
            Self::Archive => "压缩包",
            Self::Code => "代码",
            Self::Other => "其他",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Self::Folder => "📁",
            Self::Image => "🖼",
            Self::Audio => "♫",
            Self::Video => "▶",
            Self::Application => "▣",
            Self::Archive => "▤",
            Self::Code => "⌘",
            _ => "📄",
        }
    }
}
pub fn classify(name: &str, is_dir: bool) -> Category {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    if ext.eq_ignore_ascii_case("app") && is_dir {
        return Category::Application;
    }
    if is_dir {
        return Category::Folder;
    }
    // Classification only examines metadata already in memory; it opens no files.
    let groups: &[(Category, &[&str])] = &[
        (
            Category::Document,
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "pages",
                "numbers", "key", "epub", "csv", "odt", "ods", "odp",
            ],
        ),
        (
            Category::Image,
            &[
                "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "svg",
                "avif", "ico", "icns", "psd", "raw", "dng",
            ],
        ),
        (
            Category::Audio,
            &[
                "mp3", "wav", "m4a", "aac", "flac", "aiff", "ogg", "opus", "wma", "mid", "midi",
            ],
        ),
        (
            Category::Video,
            &[
                "mp4", "mov", "mkv", "avi", "webm", "m4v", "mpeg", "mpg", "ts", "wmv",
            ],
        ),
        (Category::Application, &["exe", "msi", "pkg", "dmg"]),
        (
            Category::Archive,
            &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "tgz"],
        ),
        (
            Category::Code,
            &[
                "rs", "py", "js", "ts", "tsx", "jsx", "swift", "c", "h", "cpp", "hpp", "java",
                "go", "rb", "sh", "zsh", "html", "css", "json", "yaml", "yml", "toml", "xml",
                "sql", "ipynb",
            ],
        ),
    ];
    groups
        .iter()
        .find_map(|(kind, extensions)| {
            extensions
                .iter()
                .any(|value| ext.eq_ignore_ascii_case(value))
                .then_some(*kind)
        })
        .unwrap_or(Category::Other)
}
impl CompactIndex {
    pub fn category(&self, entry_id: usize) -> Category {
        classify(self.name(entry_id), self.is_dir(entry_id))
    }
}

pub fn search(index: &CompactIndex, query: &str, limit: usize) -> Vec<usize> {
    search_filtered(index, query, limit, Category::All)
}
pub fn search_filtered(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
) -> Vec<usize> {
    search_filtered_with_scope(index, query, limit, category, true)
}

pub fn search_filtered_with_scope(
    index: &CompactIndex,
    query: &str,
    limit: usize,
    category: Category,
    include_paths: bool,
) -> Vec<usize> {
    let mut tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    if tokens.is_empty() || limit == 0 {
        return Vec::new();
    }
    tokens.truncate(8);
    let Ok(matcher) = AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .build(&tokens)
    else {
        return Vec::new();
    };
    let required = ((1u16 << tokens.len()) - 1) as u8;
    let eligible =
        |entry_id: usize| category == Category::All || index.category(entry_id) == category;
    let match_mask = |entry_id: usize| {
        let name = index.name(entry_id);
        if tokens.len() == 1 {
            return u8::from(matcher.is_match(name));
        }
        let mut mask = 0u8;
        for found in matcher.find_overlapping_iter(name) {
            mask |= 1u8 << found.pattern().as_usize();
            if mask == required {
                break;
            }
        }
        mask
    };
    // Name-only searches do not need one byte of inherited path masks per record.
    if !include_paths {
        let mut results: Vec<_> = index
            .records
            .par_iter()
            .enumerate()
            .filter_map(|(id, _)| {
                (index.is_alive(id) && eligible(id) && match_mask(id) == required).then_some(id)
            })
            .take_any(limit)
            .collect();
        results.sort_unstable();
        return results;
    }

    // Keep filename masks so path fallback never searches a name twice.
    let mut masks = vec![0u8; index.records.len()];
    let probe_len = index.records.len().min(50_000);
    let mut direct_results = Vec::with_capacity(limit.min(500));
    for entry_id in 0..probe_len {
        masks[entry_id] = match_mask(entry_id);
        if index.is_alive(entry_id) && masks[entry_id] == required && eligible(entry_id) {
            direct_results.push(entry_id);
            if direct_results.len() == limit {
                return direct_results;
            }
        }
    }
    if direct_results.len() < limit {
        let remaining = limit - direct_results.len();
        let mut parallel_results: Vec<usize> = masks[probe_len..]
            .par_iter_mut()
            .enumerate()
            .filter_map(|(offset, mask)| {
                let entry_id = probe_len + offset;
                *mask = match_mask(entry_id);
                (index.is_alive(entry_id) && *mask == required && eligible(entry_id))
                    .then_some(entry_id)
            })
            .take_any(remaining)
            .collect();
        direct_results.append(&mut parallel_results);
    }
    if direct_results.len() == limit {
        direct_results.sort_unstable();
        return direct_results;
    }

    if !include_paths {
        direct_results.sort_unstable();
        return direct_results;
    }

    // Rare queries also inherit matches from ancestor folder names, preserving path search
    // without storing every full path.
    let mut results = Vec::with_capacity(limit.min(500));
    for (entry_id, record) in index.records.iter().enumerate() {
        let inherited = if record.parent == NO_PARENT {
            0
        } else {
            masks[record.parent as usize]
        };
        let mask = masks[entry_id] | inherited;
        masks[entry_id] = mask;
        if record.flags & ALIVE != 0 && mask == required && eligible(entry_id) {
            results.push(entry_id);
            if results.len() == limit {
                break;
            }
        }
    }
    results
}

pub fn hash_path(path: &str) -> u64 {
    hash_bytes(path.as_bytes())
}
fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
fn extend_hash(mut hash: u64, name: &[u8]) -> u64 {
    for byte in name {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
fn write_u32(writer: &mut impl Write, value: u32) -> Result<(), String> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(|error| error.to_string())
}
fn write_u64(writer: &mut impl Write, value: u64) -> Result<(), String> {
    writer
        .write_all(&value.to_le_bytes())
        .map_err(|error| error.to_string())
}
fn read_u16(reader: &mut impl Read) -> Result<u16, String> {
    let mut bytes = [0; 2];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(u16::from_le_bytes(bytes))
}
fn read_u32(reader: &mut impl Read) -> Result<u32, String> {
    let mut bytes = [0; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(u32::from_le_bytes(bytes))
}
fn read_u64(reader: &mut impl Read) -> Result<u64, String> {
    let mut bytes = [0; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{CompactIndex, FlatEntry, UNKNOWN_DATE, search};
    fn sample() -> CompactIndex {
        CompactIndex::from_flat(
            vec!["/tmp".into()],
            vec![
                FlatEntry {
                    path: "/tmp".into(),
                    is_dir: true,
                    modified: UNKNOWN_DATE,
                },
                FlatEntry {
                    path: "/tmp/冷暖站".into(),
                    is_dir: true,
                    modified: UNKNOWN_DATE,
                },
                FlatEntry {
                    path: "/tmp/冷暖站/SCADA恢复记录.md".into(),
                    is_dir: false,
                    modified: UNKNOWN_DATE,
                },
                FlatEntry {
                    path: "/tmp/图片".into(),
                    is_dir: true,
                    modified: UNKNOWN_DATE,
                },
                FlatEntry {
                    path: "/tmp/图片/photo.png".into(),
                    is_dir: false,
                    modified: UNKNOWN_DATE,
                },
            ],
        )
        .unwrap()
    }
    fn temp_dir() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "suishou-date-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn journal_leaf_event_recovers_offline_tree_and_siblings_after_restart() {
        use super::*;
        let temp = temp_dir();
        let root = temp.join("home");
        fs::create_dir(&root).unwrap();
        let roots = vec![root.clone()];
        let mut index =
            CompactIndex::from_flat(roots.clone(), vec![make_flat(&root, None).unwrap()]).unwrap();
        let (snapshot, dates, journal) = (
            temp.join("index.bin"),
            temp.join("dates.bin"),
            temp.join("changes.log"),
        );
        write_snapshot(&index, &snapshot, &dates).unwrap();
        let books = root.join("orca/projects/elecr-motor/books");
        let book = books.join("暗访十年.epub");
        fs::create_dir_all(&books).unwrap();
        fs::write(&book, b"epub").unwrap();
        fs::write(root.join("orca/offline-sibling.md"), b"sibling").unwrap();
        write_journal(&journal, std::slice::from_ref(&book)).unwrap();
        index = load_compact_with_dates(&snapshot, &dates).unwrap();
        assert!(search(&index, "暗访十年", 10).is_empty());
        for _ in 0..2 {
            let scope = index.delta_scope();
            let paths = read_journal(&journal, &roots).unwrap();
            index
                .apply_delta(scope.collect_delta(paths).unwrap())
                .unwrap();
            let hits = search(&index, "暗访十年", 10);
            assert_eq!(hits.len(), 1);
            assert_eq!(index.path(hits[0]), book.to_str().unwrap());
            assert_eq!(search(&index, "offline-sibling", 10).len(), 1);
        }
        let applied = journal.metadata().unwrap().len();
        checkpoint_to(&index, applied, &snapshot, &dates, &journal).unwrap();
        assert_eq!(journal.metadata().unwrap().len(), 0);
        let mut restored = load_compact_with_dates(&snapshot, &dates).unwrap();
        fs::remove_dir_all(&books).unwrap();
        let delta = restored.delta_scope().collect_delta(vec![books]).unwrap();
        restored.apply_delta(delta).unwrap();
        assert!(search(&restored, "暗访十年", 10).is_empty());
        assert_eq!(search(&restored, "offline-sibling", 10).len(), 1);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn journal_recovers_torn_tail_and_coalesces_only_reported_ancestors() {
        use super::*;
        let root = temp_dir();
        let journal = root.join("changes.log");
        let parent = root.join("project");
        let neighbour = root.join("project-backup");
        let roots = vec![root.clone()];
        write_journal(
            &journal,
            &[
                parent.join("books/a"),
                neighbour.clone(),
                parent.join("b"),
                parent.clone(),
                parent.clone(),
                root.clone(),
                root.join("Caches/ignore"),
                PathBuf::from("/outside"),
            ],
        )
        .unwrap();
        let end = journal.metadata().unwrap().len();
        let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(&[20, 0, 0, 0, b'x']).unwrap();
        assert_eq!(
            read_journal(&journal, &roots).unwrap(),
            [parent.clone(), neighbour]
        );
        assert_eq!(journal.metadata().unwrap().len(), end);
        let independent = (0..80)
            .map(|i| root.join(format!("branch-{i}/leaf")))
            .collect::<Vec<_>>();
        write_journal(&journal, &independent).unwrap();
        acknowledge_journal(&journal, end).unwrap();
        assert_eq!(read_journal(&journal, &roots).unwrap().len(), 82);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkpoint_retains_journal_on_date_write_failure_and_restores_partial_dates() {
        use super::*;
        let root = temp_dir();
        let index_file = root.join("index.bin");
        let dates = root.join("dates.bin");
        let journal = root.join("changes.log");
        let mut index = sample();
        index.records[1].modified = UNCACHED_DATE;
        index.records[2].modified = 77;
        index.dates_ready = false;
        write_journal(&journal, &[PathBuf::from("/tmp/changed")]).unwrap();
        let end = journal.metadata().unwrap().len();
        assert!(
            checkpoint_to(
                &index,
                end,
                &index_file,
                &root.join("absent/dates"),
                &journal
            )
            .is_err()
        );
        assert_eq!(journal.metadata().unwrap().len(), end);
        checkpoint_to(&index, end, &index_file, &dates, &journal).unwrap();
        assert_eq!(journal.metadata().unwrap().len(), 0);
        let restored = load_compact_with_dates(&index_file, &dates).unwrap();
        assert!(!restored.dates_ready());
        assert_eq!(restored.modified(2), Some(77));
        assert_eq!(restored.missing_date_batch(&mut 0).len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reloaded_filesystem_root_retains_directory_hashes_for_delta_lookup() {
        use super::*;
        let root = temp_dir();
        let flat = [("/", true), ("/Users", true), ("/Users/file", false)]
            .map(|(path, is_dir)| FlatEntry {
                path: path.into(),
                is_dir,
                modified: UNKNOWN_DATE,
            })
            .to_vec();
        let index = CompactIndex::from_flat(vec!["/".into()], flat).unwrap();
        let file = root.join("snap.bin");
        write_snapshot(&index, &file, &root.join("dates.bin")).unwrap();
        let restored = load_compact(&file).unwrap();
        assert_eq!(restored.path_hashes, index.path_hashes);
        assert_eq!(restored.directory_ids, index.directory_ids);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watcher_noise_respects_explicit_roots_and_path_boundaries() {
        use super::*;
        let roots = vec![PathBuf::from("/")];
        for path in [
            "/private/var/folders/x",
            "/private/tmp",
            "/System/Volumes/Update/a",
            "/Users/test/Library/Caches/a",
            "/Volumes/data/Caches/x",
            "/Users/test/Library/Application Support/com.gkgy.MiaoFind/index-v3.tmp",
            "/Users/test/Library/Application Support/随手找/app.ron",
        ] {
            assert!(!sync_path(Path::new(path), &roots), "{path}");
        }
        for path in [
            "/Users/test/orca/file",
            "/Volumes/data/Models/a",
            "/Users/test/Caches-old/x",
            "/private/tmp-notes",
        ] {
            assert!(sync_path(Path::new(path), &roots), "{path}");
        }
        let custom = PathBuf::from("/private/tmp/project/Caches");
        assert!(sync_path(
            &custom.join("source"),
            std::slice::from_ref(&custom)
        ));
        assert!(!sync_path(Path::new("/private/tmp/other"), &[custom]));
    }

    #[test]
    fn delta_skips_cache_contents_without_losing_full_disk_records() {
        use super::*;
        let root = temp_dir();
        let project = root.join("project");
        let cache = project.join("Caches");
        fs::create_dir_all(&cache).unwrap();
        let old = cache.join("old-cache");
        let fresh = cache.join("fresh-cache");
        fs::write(&old, b"").unwrap();
        let roots = vec![root.clone()];
        let flat = [&root, &project, &cache, &old]
            .map(|p| make_flat(p, None).unwrap())
            .to_vec();
        let mut index = CompactIndex::from_flat(roots, flat).unwrap();
        fs::write(&fresh, b"").unwrap();
        fs::write(project.join("project-document"), b"").unwrap();
        index
            .apply_delta(
                index
                    .delta_scope()
                    .collect_delta(vec![project.clone()])
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(search(&index, "old-cache", 10).len(), 1);
        assert!(search(&index, "fresh-cache", 10).is_empty());
        assert_eq!(search(&index, "project-document", 10).len(), 1);
        fs::remove_dir_all(project).unwrap();
        index
            .apply_delta(
                index
                    .delta_scope()
                    .collect_delta(vec![root.join("project")])
                    .unwrap(),
            )
            .unwrap();
        assert!(search(&index, "old-cache", 10).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn personal_apps_show_bundles_in_both_locations_and_hide_internals() {
        use super::*;
        let root = temp_dir();
        let system = root.join("Applications");
        let home = root.join("home");
        let user_apps = home.join("Applications");
        let cache = home.join("Caches");
        let mut flat: Vec<_> = [&root, &system, &home, &user_apps, &cache]
            .map(|p| make_flat(p, Some(true)).unwrap())
            .to_vec();
        for dir in [&user_apps, &system] {
            let app = dir.join("Editor.app");
            flat.push(make_flat(&app, Some(true)).unwrap());
            flat.push(make_flat(&app.join("Contents"), Some(true)).unwrap());
        }
        let index = CompactIndex::from_flat(vec![root.clone()], flat).unwrap();
        let roots = [system, home];
        let mask = index.personal_mask(&roots);
        let visible = |name, mask: &[bool]| {
            search(&index, name, 20)
                .into_iter()
                .filter(|&id| index.name(id) == name && mask[id])
                .count()
        };
        assert_eq!(visible("Editor.app", &mask), 2);
        assert_eq!(visible("Contents", &mask), 0);
        assert_eq!(visible("Caches", &mask), 0);
        let explicit = index.personal_mask(&[user_apps.join("Editor.app")]);
        assert_eq!(visible("Contents", &explicit), 1);
        assert_eq!(
            visible(
                "Contents",
                &index.personal_mask_with_exclusions(&roots, &[])
            ),
            2
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relevant_main_apps_precede_helpers_but_manual_dates_stay_global() {
        use super::{Category, SortOrder, search_sorted_with_scope};
        let mut flat: Vec<_> = [
            "/",
            "/Applications",
            "/Applications/Google Chrome.app/Contents",
            "/private",
            "/private/var",
            "/private/var/folders",
            "/private/var/folders/cache",
            "/tmp",
        ]
        .into_iter()
        .map(|path| FlatEntry {
            path: path.into(),
            is_dir: true,
            modified: 0,
        })
        .collect();
        flat.extend(vec![
            FlatEntry {
                path: "/Applications/Google Chrome.app".into(),
                is_dir: true,
                modified: 100,
            },
            FlatEntry {
                path: "/Applications/Google Chrome.app/Contents/Google Chrome Helper.app".into(),
                is_dir: true,
                modified: 900,
            },
            FlatEntry {
                path: "/private/var/folders/cache/Google Chrome.app".into(),
                is_dir: true,
                modified: 1000,
            },
            FlatEntry {
                path: "/Applications/WorkBuddy.app".into(),
                is_dir: true,
                modified: 200,
            },
            FlatEntry {
                path: "/tmp/WorkBuddy Helper.app".into(),
                is_dir: true,
                modified: 2000,
            },
        ]);
        let index = CompactIndex::from_flat(vec!["/".into()], flat).unwrap();
        let run = |q, order| {
            search_sorted_with_scope(&index, q, 500, Category::Application, order, false)
        };
        assert_eq!(
            index.path(run("chrome", SortOrder::Relevance)[0]),
            "/Applications/Google Chrome.app"
        );
        assert_eq!(
            index.path(run("work buddy", SortOrder::Relevance)[0]),
            "/Applications/WorkBuddy.app"
        );
        assert_eq!(
            index.modified(run("chrome", SortOrder::Latest)[0]),
            Some(1000)
        );
        assert_eq!(
            index.modified(run("chrome", SortOrder::Oldest)[0]),
            Some(100)
        );
        let limited = search_sorted_with_scope(
            &index,
            "chrome",
            1,
            Category::Application,
            SortOrder::Relevance,
            false,
        );
        assert_eq!(index.path(limited[0]), "/Applications/Google Chrome.app");
    }
    #[test]
    fn name_scope_excludes_parent_only_matches_with_sort_and_category() {
        use super::{Category, SortOrder, search_sorted_with_scope};
        let index = sample();
        for order in [SortOrder::Latest, SortOrder::Oldest, SortOrder::Name] {
            let names =
                search_sorted_with_scope(&index, "冷暖站", 500, Category::All, order, false);
            assert_eq!(
                names.iter().map(|id| index.name(*id)).collect::<Vec<_>>(),
                vec!["冷暖站"]
            );
            assert!(
                search_sorted_with_scope(&index, "冷暖站", 500, Category::Document, order, false)
                    .is_empty()
            );
            let paths =
                search_sorted_with_scope(&index, "冷暖站", 500, Category::Document, order, true);
            assert_eq!(paths.len(), 1);
            assert_eq!(index.name(paths[0]), "SCADA恢复记录.md");
            assert!(
                search_sorted_with_scope(&index, "冷暖站 md", 500, Category::All, order, false)
                    .is_empty()
            );
            assert_eq!(
                search_sorted_with_scope(&index, "冷暖站 md", 500, Category::All, order, true)
                    .len(),
                1
            );
            assert_eq!(
                search_sorted_with_scope(&index, "scada MD", 500, Category::Document, order, false)
                    .len(),
                1
            );
        }
    }
    #[test]
    fn newest_is_selected_before_limit_and_unknown_dates_are_last() {
        use super::{Category, SortOrder, search_sorted};
        let mut flat = vec![FlatEntry {
            path: "/tmp".into(),
            is_dir: true,
            modified: UNKNOWN_DATE,
        }];
        for n in 0..800 {
            flat.push(FlatEntry {
                path: format!("/tmp/report{n:04}.md").into(),
                is_dir: false,
                modified: n as i64,
            });
        }
        flat.push(FlatEntry {
            path: "/tmp/report-unknown.md".into(),
            is_dir: false,
            modified: UNKNOWN_DATE,
        });
        let index = CompactIndex::from_flat(vec!["/tmp".into()], flat).unwrap();
        let newest = search_sorted(&index, "report", 500, Category::Document, SortOrder::Latest);
        assert_eq!(newest.len(), 500);
        assert_eq!(index.name(newest[0]), "report0799.md");
        assert_eq!(index.modified(*newest.last().unwrap()), Some(300));
        let oldest = search_sorted(
            &index,
            "report",
            usize::MAX,
            Category::Document,
            SortOrder::Oldest,
        );
        assert_eq!(index.name(oldest[0]), "report0000.md");
        assert_eq!(index.modified(*oldest.last().unwrap()), None);
        let named = search_sorted(&index, "report", 500, Category::Document, SortOrder::Name);
        assert!(
            named
                .windows(2)
                .all(|pair| index.name(pair[0]) <= index.name(pair[1]))
        );
        assert_eq!(index.name(named[0]), "report-unknown.md");
        assert!(search_sorted(&index, "report", 0, Category::All, SortOrder::Latest).is_empty());
    }
    #[test]
    fn sidecar_matches_by_path_hash_and_survives_record_reordering() {
        let dir = temp_dir();
        let cache = dir.join("dates.bin");
        let mut index = sample();
        index.records[2].modified = 123456789;
        super::save_dates_to(&index, &cache).unwrap();
        let mut loaded = sample();
        loaded.records.swap(2, 4);
        loaded.path_hashes.swap(2, 4);
        super::load_dates(&mut loaded, &cache).unwrap();
        assert_eq!(loaded.modified(4), Some(123456789));
        assert!(loaded.dates_ready());
        std::fs::write(&cache, b"broken").unwrap();
        assert!(super::load_dates(&mut loaded, &cache).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn aligned_cache_skips_stat_even_for_unavailable_dates_and_partial_cache_repairs_only_gaps() {
        let dir = temp_dir();
        let cache = dir.join("dates.bin");
        let mut original = sample();
        original.records[2].modified = 1234;
        super::save_dates_to(&original, &cache).unwrap();
        let mut loaded = sample();
        loaded.dates_ready = false;
        super::load_dates(&mut loaded, &cache).unwrap();
        assert!(loaded.dates_ready());
        assert!(loaded.missing_date_batch(&mut 0).is_empty());
        assert_eq!(loaded.modified(2), Some(1234));
        // A valid cache for a slightly older index still covers all but the last record.
        let n = loaded.len();
        let mut partial = std::fs::read(&cache).unwrap();
        partial[8..16].copy_from_slice(&((n - 1) as u64).to_le_bytes());
        partial.truncate(partial.len() - 16);
        std::fs::write(&cache, partial).unwrap();
        super::load_dates(&mut loaded, &cache).unwrap();
        assert!(!loaded.dates_ready());
        let mut cursor = 0;
        assert_eq!(
            loaded.missing_date_batch(&mut cursor),
            vec![(n - 1, loaded.path(n - 1))]
        );
        loaded.apply_date_batch(vec![(n - 1, 5678)]);
        assert!(loaded.missing_date_batch(&mut cursor).is_empty());
        assert_eq!(loaded.modified(n - 1), Some(5678));
        assert_eq!(loaded.modified(2), Some(1234));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn filesystem_modified_time_and_concurrent_updates_are_preserved() {
        let dir = temp_dir();
        let file = dir.join("test.md");
        let handle = std::fs::File::create(&file).unwrap();
        handle
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(123456)),
            )
            .unwrap();
        let entry = super::make_flat(&file, Some(false)).unwrap();
        assert_eq!(entry.modified, 123456000);
        let mut index = sample();
        index.records[2].modified = 200;
        index.apply_date_batch(vec![(2, 100)]);
        assert_eq!(index.modified(2), Some(200));
        index.records[2].modified = super::UNCACHED_DATE;
        index.apply_date_batch(vec![(2, 100)]);
        assert_eq!(index.modified(2), Some(100));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn classifies_metadata_and_app_bundles() {
        use super::{Category::*, classify};
        assert_eq!(classify("报告.PDF", false), Document);
        assert_eq!(classify("照片.HEIC", false), Image);
        assert_eq!(classify("录音.flac", false), Audio);
        assert_eq!(classify("成片.mov", false), Video);
        assert_eq!(classify("秒找.app", true), Application);
        assert_eq!(classify("资料.pdf", true), Folder);
        assert_eq!(classify("backup.tar.gz", false), Archive);
        assert_eq!(classify("main.RS", false), Code);
        assert_eq!(classify("README", false), Other);
    }
    #[test]
    fn filter_searches_beyond_unfiltered_limit_and_parallel_boundary() {
        use super::{Category, search_filtered};
        let mut flat = vec![FlatEntry {
            path: "/tmp".into(),
            is_dir: true,
            modified: UNKNOWN_DATE,
        }];
        for n in 0..60_000 {
            flat.push(FlatEntry {
                path: format!("/tmp/common{n:05}.txt").into(),
                is_dir: false,
                modified: UNKNOWN_DATE,
            });
        }
        flat.push(FlatEntry {
            path: "/tmp/z-common.PNG".into(),
            is_dir: false,
            modified: UNKNOWN_DATE,
        });
        let mut index = CompactIndex::from_flat(vec!["/tmp".into()], flat).unwrap();
        let found = search_filtered(&index, "common", 1, Category::Image);
        assert_eq!(found.len(), 1);
        assert_eq!(index.name(found[0]), "z-common.PNG");
        assert_eq!(
            search_filtered(&index, "tmp PNG", 500, Category::Image),
            found
        );
        index
            .apply_delta(super::IndexDelta {
                removed_prefixes: vec!["/tmp/z-common.PNG".into()],
                additions: vec![],
                changed: 1,
                opaque: Default::default(),
            })
            .unwrap();
        assert!(search_filtered(&index, "common", 1, Category::Image).is_empty());
    }
    #[test]
    fn overlapping_tokens_and_ancestor_filter() {
        use super::{Category, search_filtered};
        let index = sample();
        assert_eq!(search(&index, "scada cada", 500).len(), 1);
        assert_eq!(
            search_filtered(&index, "冷暖站 md", 500, Category::Document).len(),
            1
        );
        assert!(search_filtered(&index, "冷暖站 md", 500, Category::Folder).is_empty());
        assert_eq!(
            search_filtered(&index, "图片", 500, Category::Image).len(),
            1
        );
        assert!(search(&index, "photo", 0).is_empty());
    }
    #[test]
    fn reconstructs_paths() {
        let index = sample();
        assert_eq!(index.path(2), "/tmp/冷暖站/SCADA恢复记录.md");
    }
    #[test]
    fn searches_names_and_ancestors() {
        let index = sample();
        assert_eq!(search(&index, "scada", 20).len(), 1);
        assert_eq!(search(&index, "冷暖站 md", 20).len(), 1);
        assert_eq!(search(&index, "tmp photo", 20).len(), 1);
        assert!(search(&index, "不存在", 20).is_empty());
    }
    #[test]
    fn respects_limit() {
        let index = sample();
        assert_eq!(search(&index, "tmp", 1).len(), 1);
    }
    #[test]
    fn removes_nested_roots() {
        let roots = super::normalize_roots(vec![
            "/Users".into(),
            "/Users/demo/Documents".into(),
            "/tmp".into(),
        ]);
        assert!(
            roots
                .iter()
                .any(|root| root == std::path::Path::new("/Users"))
        );
        assert!(
            !roots
                .iter()
                .any(|root| root == std::path::Path::new("/Users/demo/Documents"))
        );
    }
}

#[cfg(test)]
mod personal_tests {
    use super::*;
    #[test]
    fn scope_date_and_path_filter_before_limit() {
        let paths = [
            ("/Users/test/Documents/work/output/recent.png", 900),
            ("/Users/test/Documents/work/output/old.png", 100),
            ("/Users/test/Documents/work/comfy-output/baby.png", 850),
            ("/Users/test/Documents/work/node_modules/output/no.png", 999),
            ("/Users/test/Library/output/no.png", 1000),
            ("/System/output/no.png", 1000),
            ("/Users/test/Documents/work/output2/also.png", 820),
            ("/Users/test/Documents/work/output/icon.svg", 810),
            (
                "/Users/test/Documents/work/output/unknown.png",
                UNKNOWN_DATE,
            ),
            ("/Users/test/Documents/work/dist/output/no.png", 999),
            (
                "/Users/test/Documents/work/Test.app/Contents/output/no.png",
                999,
            ),
        ];
        let mut flat = Vec::new();
        let mut dirs = HashSet::new();
        for (path, date) in paths {
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent {
                dirs.insert(p.to_string_lossy().to_string());
                parent = p.parent();
            }
            flat.push(FlatEntry {
                path: path.into(),
                is_dir: false,
                modified: date,
            });
        }
        flat.extend(dirs.into_iter().map(|p| FlatEntry {
            path: p.into(),
            is_dir: true,
            modified: 0,
        }));
        let index = CompactIndex::from_flat(vec![PathBuf::from("/")], flat).unwrap();
        let mask = index.personal_mask(&[PathBuf::from("/Users/test/Documents")]);
        let run = |q, limit| {
            search_with_filters(
                &index,
                q,
                limit,
                Category::Image,
                SortOrder::Latest,
                false,
                Some(&mask),
                Some(800),
            )
        };
        for query in ["", "/output", "png /output"] {
            let hits = search_with_date_range(
                &index,
                query,
                1,
                Category::Image,
                SortOrder::Latest,
                false,
                Some(&mask),
                Some(800),
                Some(900),
                true,
            );
            assert_eq!(hits.len(), 1);
            assert_eq!(index.name(hits[0]), "baby.png");
        }
        assert_eq!(index.name(run("/output", 1)[0]), "recent.png");
        assert_eq!(run("/output", 500).len(), 4);
        assert_eq!(index.name(run("baby /output", 500)[0]), "baby.png");
        assert_eq!(run("baby /output", 500), run("/output baby", 500));
        assert_eq!(
            run("baby /work /output", 500),
            run("/output baby /work", 500)
        );
        assert!(run("baby output", 500).is_empty());
        assert_eq!(run("", 500).len(), 4);
        assert_eq!(
            search_with_options(
                &index,
                "/output",
                500,
                Category::Image,
                SortOrder::Latest,
                false,
                Some(&mask),
                Some(800),
                true
            )
            .len(),
            3
        );
        let mask = index.personal_mask(&[
            PathBuf::from("/Users/test/Documents"),
            PathBuf::from("/Users/test/Documents/work/node_modules/output"),
        ]);
        assert_eq!(
            search_with_filters(
                &index,
                "",
                500,
                Category::Image,
                SortOrder::Latest,
                false,
                Some(&mask),
                Some(800)
            )
            .len(),
            5
        );
    }
}

#[cfg(test)]
mod paging_tests {
    use super::*;
    fn fixture() -> CompactIndex {
        let mut flat = vec![FlatEntry {
            path: "/tmp".into(),
            is_dir: true,
            modified: UNKNOWN_DATE,
        }];
        for i in 0..1733 {
            flat.push(FlatEntry {
                path: format!(
                    "/tmp/item-{:04}.{}",
                    (i * 37) % 1733,
                    if i % 3 == 0 { "png" } else { "md" }
                )
                .into(),
                is_dir: false,
                modified: if i % 17 == 0 {
                    UNKNOWN_DATE
                } else {
                    (i % 100) as i64
                },
            });
        }
        let mut index = CompactIndex::from_flat(vec!["/tmp".into()], flat).unwrap();
        index.dates_ready = true;
        index
    }
    #[test]
    fn pages_match_global_sort_and_arbitrary_jumps_without_duplicates() {
        let index = fixture();
        for order in [
            SortOrder::Relevance,
            SortOrder::Name,
            SortOrder::Latest,
            SortOrder::Oldest,
        ] {
            let expected = search_with_date_range(
                &index,
                "item",
                usize::MAX,
                Category::All,
                order,
                false,
                None,
                None,
                None,
                false,
            );
            let mut session = SearchSession::new(
                &index,
                "item",
                Category::All,
                order,
                false,
                None,
                None,
                None,
                false,
            );
            assert_eq!(session.total(), 1733);
            for page in [3, 0, 2, 1, 3, 1, 0] {
                assert_eq!(
                    session.page(page, 500),
                    expected[page * 500..expected.len().min((page + 1) * 500)]
                );
            }
            let actual: Vec<_> = (0..4).flat_map(|p| session.page(p, 500)).collect();
            assert_eq!(actual, expected);
            assert_eq!(actual.iter().copied().collect::<HashSet<_>>().len(), 1733);
            assert!(session.page(4, 500).is_empty());
            assert!(session.page(usize::MAX, 500).is_empty());
            assert!(session.page(0, 0).is_empty());
        }
    }
    #[test]
    fn filters_apply_before_paging_and_explicit_empty_type_browses() {
        let index = fixture();
        let mask: Vec<_> = (0..index.records.len()).map(|id| id % 2 == 0).collect();
        let order = SortOrder::Latest;
        let expected = search_with_date_range(
            &index,
            "/tmp item",
            usize::MAX,
            Category::Image,
            order,
            false,
            Some(&mask),
            Some(10),
            Some(80),
            true,
        );
        let mut session = SearchSession::new(
            &index,
            "/tmp item",
            Category::Image,
            order,
            false,
            Some(&mask),
            Some(10),
            Some(80),
            true,
        );
        assert_eq!(session.total(), expected.len());
        assert_eq!(session.page(0, 500), expected);
        let mut session = SearchSession::new(
            &index,
            "",
            Category::Image,
            order,
            false,
            None,
            None,
            None,
            true,
        );
        assert_eq!(session.total(), 578);
        assert_eq!(session.page(1, 500).len(), 78);
        assert_eq!(
            SearchSession::new(
                &index,
                "",
                Category::All,
                order,
                false,
                None,
                None,
                None,
                false
            )
            .total(),
            0
        );
    }
}

#[cfg(test)]
mod name_snapshot_tests {
    use super::*;
    #[test]
    fn shared_names_survive_appends_chunk_boundaries_and_flat_persistence() {
        let mut names = NameStore::from(b"base".to_vec());
        let base = names.clone();
        names.prepare_name(3);
        let one = names.len();
        names.extend_from_slice(b"one");
        let frozen = names.clone();
        names.prepare_name(3);
        names.extend_from_slice(b"two");
        assert_eq!(base.len(), 4);
        assert_eq!(frozen.len(), 7);
        assert_eq!(frozen.slice(one, 3), b"one");
        let filler = vec![b'x'; NAME_CHUNK - 8];
        names.prepare_name(filler.len());
        names.extend_from_slice(&filler);
        assert_eq!(names.len(), 4 + NAME_CHUNK - 2);
        names.prepare_name(3);
        let next = names.len();
        names.extend_from_slice(b"end");
        assert_eq!(next, 4 + NAME_CHUNK);
        assert_eq!(names.slice(next, 3), b"end");
        assert_eq!(frozen.slice(one, 3), b"one");
        let mut bytes = Vec::new();
        names.write_to(&mut bytes).unwrap();
        assert_eq!(bytes.len(), names.len());
        let loaded = NameStore::from(bytes);
        assert_eq!(loaded.slice(one, 3), b"one");
        assert_eq!(loaded.slice(next, 3), b"end");
    }
}
