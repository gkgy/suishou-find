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
    time::Instant,
};

use aho_corasick::AhoCorasickBuilder;
use directories::ProjectDirs;
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;
use rusqlite::Connection;

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
}

#[derive(Debug, Default)]
pub struct CompactIndex {
    records: Vec<Record>,
    names: Vec<u8>,
    path_hashes: Vec<u64>,
    roots: Vec<PathBuf>,
    active_count: usize,
}

#[derive(Clone, Debug)]
struct FlatEntry {
    path: Box<str>,
    is_dir: bool,
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
    let path = compact_index_path()?;
    let mut index = if path.exists() {
        load_compact(&path)?
    } else if legacy_path()?.exists() {
        migrate_legacy()?
    } else {
        CompactIndex::default()
    };
    let paths = read_journal()?;
    if !paths.is_empty() {
        index.apply_delta(collect_delta(paths, false)?)?;
    }
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
            })
        })
        .map_err(|error| error.to_string())?;
    let mut flat = Vec::new();
    for row in rows {
        flat.push(row.map_err(|error| error.to_string())?);
    }
    let index = CompactIndex::from_flat(roots, flat)?;
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
    Ok(())
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
    let is_dir = known_is_dir.unwrap_or_else(|| path.is_dir());
    Some(FlatEntry {
        path: value.into_boxed_str(),
        is_dir,
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

pub fn search(index: &CompactIndex, query: &str, limit: usize) -> Vec<usize> {
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
    let direct_match = |entry_id: usize| {
        if !index.is_alive(entry_id) {
            return false;
        }
        let mut mask = 0u8;
        for found in matcher.find_overlapping_iter(index.name(entry_id)) {
            mask |= 1u8 << found.pattern().as_usize();
            if mask == required {
                return true;
            }
        }
        false
    };
    let probe_len = index.records.len().min(50_000);
    let mut direct_results: Vec<usize> = (0..probe_len)
        .filter(|&entry_id| direct_match(entry_id))
        .take(limit)
        .collect();
    if direct_results.len() < limit {
        let remaining = limit - direct_results.len();
        let mut parallel_results: Vec<usize> = (probe_len..index.records.len())
            .into_par_iter()
            .filter(|&entry_id| direct_match(entry_id))
            .take_any(remaining)
            .collect();
        direct_results.append(&mut parallel_results);
    }
    if direct_results.len() == limit {
        direct_results.sort_unstable();
        return direct_results;
    }

    // Rare queries also inherit matches from ancestor folder names, preserving path search
    // without storing every full path.
    let mut masks = Vec::<u8>::with_capacity(index.records.len());
    let mut results = Vec::with_capacity(limit.min(500));
    for (entry_id, record) in index.records.iter().enumerate() {
        let mut mask = if record.parent == NO_PARENT {
            0
        } else {
            masks[record.parent as usize]
        };
        for found in matcher.find_overlapping_iter(index.name(entry_id)) {
            mask |= 1u8 << found.pattern().as_usize();
        }
        masks.push(mask);
        if record.flags & ALIVE != 0 && mask == required {
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
    use super::{CompactIndex, FlatEntry, search};
    fn sample() -> CompactIndex {
        CompactIndex::from_flat(
            vec!["/tmp".into()],
            vec![
                FlatEntry {
                    path: "/tmp".into(),
                    is_dir: true,
                },
                FlatEntry {
                    path: "/tmp/冷暖站".into(),
                    is_dir: true,
                },
                FlatEntry {
                    path: "/tmp/冷暖站/SCADA恢复记录.md".into(),
                    is_dir: false,
                },
                FlatEntry {
                    path: "/tmp/图片".into(),
                    is_dir: true,
                },
                FlatEntry {
                    path: "/tmp/图片/photo.png".into(),
                    is_dir: false,
                },
            ],
        )
        .unwrap()
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
