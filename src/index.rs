use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
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
const DATE_MAGIC: &[u8; 8] = b"MIAODATE";
const MAGIC: &[u8; 8] = b"MIAOIDX3";
const NO_PARENT: u32 = u32::MAX;
const ALIVE: u8 = 1;
const DIRECTORY: u8 = 2;

#[derive(Clone, Copy, Debug)]
struct Record {
    parent: u32,
    name_start: u32,
    name_len: u16,
    flags: u8,
    modified: i64,
}

#[derive(Clone, Debug, Default)]
pub struct CompactIndex {
    records: Vec<Record>,
    names: Vec<u8>,
    path_hashes: Vec<u64>,
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
    fn from_flat(roots: Vec<PathBuf>, mut flat: Vec<FlatEntry>) -> Result<Self, String> {
        flat.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        flat.dedup_by(|a, b| a.path == b.path);
        let mut index = Self {
            records: Vec::with_capacity(flat.len()),
            names: Vec::with_capacity(flat.len().saturating_mul(20)),
            path_hashes: Vec::with_capacity(flat.len()),
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
        let needed: HashSet<u64> = flat
            .iter()
            .filter_map(|entry| Path::new(entry.path.as_ref()).parent())
            .filter_map(Path::to_str)
            .map(hash_path)
            .collect();
        let mut directory_ids = HashMap::<u64, u32>::with_capacity(needed.len() * 2 + 16);
        for (id, (&hash, record)) in self.path_hashes.iter().zip(&self.records).enumerate() {
            if record.flags & (ALIVE | DIRECTORY) == ALIVE | DIRECTORY && needed.contains(&hash) {
                directory_ids.insert(hash, id as u32);
            }
        }
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
                    continue;
                };
                let name = value
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| format!("无法读取文件名：{path}"))?;
                (parent, name)
            };
            if name.len() > u16::MAX as usize
                || self.names.len().saturating_add(name.len()) > u32::MAX as usize
            {
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
        let removed: HashSet<u64> = delta
            .removed_prefixes
            .iter()
            .map(|path| hash_path(path))
            .collect();
        for index in 0..self.records.len() {
            let parent_removed = self.records[index].parent != NO_PARENT
                && !self.is_alive(self.records[index].parent as usize);
            if self.is_alive(index)
                && (removed.contains(&self.path_hashes[index]) || parent_removed)
            {
                self.records[index].flags &= !ALIVE;
                self.active_count -= 1;
            }
        }
        self.append_flat(delta.additions)
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
            self.names = names;
            self.path_hashes = path_hashes;
            self.active_count = self.records.len();
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
        (self.records[id].modified != UNKNOWN_DATE).then_some(self.records[id].modified)
    }
    pub fn dates_ready(&self) -> bool {
        self.dates_ready
    }
    pub fn collect_dates(&self) -> Vec<(u64, i64, i64)> {
        self.records
            .par_iter()
            .enumerate()
            .filter(|(_id, r)| r.flags & ALIVE != 0)
            .map(|(id, _)| {
                let date = fs::symlink_metadata(self.path(id))
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(timestamp_ms)
                    .unwrap_or(UNKNOWN_DATE);
                (self.path_hashes[id], self.records[id].modified, date)
            })
            .collect()
    }
    pub fn apply_dates(&mut self, values: Vec<(u64, i64, i64)>) {
        let dates: HashMap<_, _> = values
            .into_iter()
            .map(|(hash, before, after)| (hash, (before, after)))
            .collect();
        for (hash, record) in self.path_hashes.iter().zip(&mut self.records) {
            // Preserve newer metadata obtained by a concurrent file change.
            if let Some(&(before, after)) = dates.get(hash) {
                if record.modified == before {
                    record.modified = after;
                }
            }
        }
        self.dates_ready = true;
    }
    pub fn save_dates(&self) -> Result<(), String> {
        if !self.dates_ready {
            return Ok(());
        }
        save_dates_to(self, &data_dir()?.join("modified-v1.bin"))
    }
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
    writer.flush().map_err(|e| e.to_string())?;
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
        .all(|(hash, _)| dates.contains_key(hash));
    for (hash, record) in index.path_hashes.iter().zip(&mut index.records) {
        if let Some(&date) = dates.get(hash) {
            record.modified = date;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Latest,
    Oldest,
    Name,
}
impl SortOrder {
    pub fn label(self) -> &'static str {
        match self {
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
    if order != SortOrder::Name && !index.dates_ready {
        return search_filtered_with_scope(index, query, limit, category, include_paths);
    }
    if limit == 0 {
        return Vec::new();
    }
    let mut results = search_filtered_with_scope(index, query, usize::MAX, category, include_paths);
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
fn local_external_volumes() -> Vec<PathBuf> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo returns a system-owned array with the returned count.
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Vec::new();
    }
    let mut volumes = Vec::new();
    for index in 0..count as usize {
        // SAFETY: index is bounded by the returned count.
        let mount = unsafe { &*mounts.add(index) };
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
fn local_external_volumes() -> Vec<PathBuf> {
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
    Ok(data_dir()?.join("changes-v3.log"))
}
fn legacy_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("index.sqlite3"))
}

pub fn load() -> Result<CompactIndex, String> {
    let mut index = load_cached()?;
    index.apply_delta(pending_journal_delta()?)?;
    Ok(index)
}

pub fn pending_journal_delta() -> Result<IndexDelta, String> {
    collect_delta(read_journal()?, false)
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
                modified: UNKNOWN_DATE,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut flat = Vec::new();
    for row in rows {
        flat.push(row.map_err(|error| error.to_string())?);
    }
    let mut index = CompactIndex::from_flat(roots, flat)?;
    index.dates_ready = false;
    save_compact(&index)?;
    Ok(index)
}

fn save_compact(index: &CompactIndex) -> Result<(), String> {
    let path = compact_index_path()?;
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
            .and_then(|_| writer.write_all(&[record.flags, 0]))
            .map_err(|error| error.to_string())?;
    }
    writer
        .write_all(&index.names)
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())?;
    fs::rename(&temporary, &path).map_err(|error| error.to_string())?;
    File::create(journal_path()?).map_err(|error| error.to_string())?;
    index.save_dates()?;
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
    let mut records = Vec::with_capacity(count);
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
            flags: flags[0],
            modified: UNKNOWN_DATE,
        });
    }
    let mut names = vec![0; names_len];
    reader
        .read_exact(&mut names)
        .map_err(|error| error.to_string())?;
    std::str::from_utf8(&names).map_err(|error| error.to_string())?;
    let mut path_hashes = Vec::with_capacity(count);
    for (index, record) in records.iter().enumerate() {
        let start = record.name_start as usize;
        let end = start + record.name_len as usize;
        if end > names.len() || (record.parent != NO_PARENT && record.parent as usize >= index) {
            return Err("索引记录损坏，需要重新建立索引".to_owned());
        }
        let hash = if record.parent == NO_PARENT {
            hash_bytes(&names[start..end])
        } else {
            extend_hash(path_hashes[record.parent as usize], &names[start..end])
        };
        path_hashes.push(hash);
    }
    let active_count = records
        .iter()
        .filter(|record| record.flags & ALIVE != 0)
        .count();
    Ok(CompactIndex {
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

pub fn update_paths(paths: Vec<PathBuf>, sender: Sender<Result<IndexDelta, String>>) {
    std::thread::spawn(move || {
        let _ = sender.send(collect_delta(paths, true));
    });
}

fn collect_delta(paths: Vec<PathBuf>, persist: bool) -> Result<IndexDelta, String> {
    let mut paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| {
            !path
                .to_string_lossy()
                .contains("/Application Support/com.gkgy.MiaoFind/")
        })
        .collect();
    paths.sort();
    paths.dedup();
    if persist && !paths.is_empty() {
        append_journal(&paths)?;
    }
    let mut removed_prefixes = Vec::with_capacity(paths.len());
    let mut additions = Vec::new();
    for path in &paths {
        removed_prefixes.push(path.to_string_lossy().into_owned());
        if path.is_dir() {
            for item in WalkBuilder::new(path)
                .hidden(false)
                .git_ignore(false)
                .git_global(false)
                .git_exclude(false)
                .follow_links(false)
                .filter_entry(|entry| !should_skip(entry.path()))
                .build()
                .flatten()
            {
                if let Some(entry) =
                    make_flat(item.path(), item.file_type().map(|kind| kind.is_dir()))
                {
                    additions.push(entry);
                }
            }
        } else if path.exists() {
            if let Some(entry) = make_flat(path, Some(false)) {
                additions.push(entry);
            }
        }
    }
    let changed = removed_prefixes.len() + additions.len();
    Ok(IndexDelta {
        removed_prefixes,
        additions,
        changed,
    })
}

fn append_journal(paths: &[PathBuf]) -> Result<(), String> {
    let mut writer = BufWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(journal_path()?)
            .map_err(|error| error.to_string())?,
    );
    for path in paths {
        let value = path.to_string_lossy();
        write_u32(&mut writer, value.len() as u32)?;
        writer
            .write_all(value.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn read_journal() -> Result<Vec<PathBuf>, String> {
    let path = journal_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut reader = BufReader::new(File::open(path).map_err(|error| error.to_string())?);
    let mut paths = Vec::new();
    loop {
        let mut length = [0; 4];
        match reader.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        }
        let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
        match reader.read_exact(&mut bytes) {
            Ok(()) => paths.push(PathBuf::from(
                String::from_utf8(bytes).map_err(|error| error.to_string())?,
            )),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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

fn hash_path(path: &str) -> u64 {
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
    hash ^= u64::from(b'/');
    hash = hash.wrapping_mul(0x100000001b3);
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
        let path = std::env::temp_dir().join(format!(
            "suishou-date-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
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
        let hash = index.path_hashes[2];
        index.records[2].modified = 200;
        index.apply_dates(vec![(hash, UNKNOWN_DATE, 100)]);
        assert_eq!(index.modified(2), Some(200));
        index.records[2].modified = UNKNOWN_DATE;
        index.apply_dates(vec![(hash, UNKNOWN_DATE, 100)]);
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
