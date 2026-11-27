// Replay the historical part of FSEvents between launches. Keep structural events
// in the regular delta journal before advancing past an incomplete replay.
use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString, c_char, c_void},
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use miao_find::index::{self, CompactIndex};

unsafe extern "C" {
    fn FSEventsGetCurrentEventId() -> u64;
    fn miao_replay_history(
        since: u64,
        roots: *const *const c_char,
        count: usize,
        sink: extern "C" fn(*mut c_void, *const c_char, bool) -> bool,
        state: *mut c_void,
    ) -> bool;
}

#[derive(Default)]
struct Changes {
    roots: Vec<PathBuf>,
    dates: HashMap<u64, Option<i64>>,
    structural: HashSet<PathBuf>,
}
extern "C" fn collect(state: *mut c_void, path: *const c_char, structural: bool) -> bool {
    // C calls this synchronously on the worker thread with a NUL-terminated path.
    let changes = unsafe { &mut *state.cast::<Changes>() };
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else {
        return true;
    };
    let hash = index::hash_path(path);
    let path = Path::new(path);
    if !index::sync_path(path, &changes.roots) {
        return true;
    }
    if !changes.dates.contains_key(&hash) {
        if changes.dates.len() >= 250_000 {
            return false;
        }
        changes.dates.insert(hash, index::file_date(path));
    }
    if structural && !changes.roots.iter().any(|r| r == path) {
        changes.structural.insert(path.to_owned());
    }
    true
}

#[derive(Default)]
struct Cursor {
    id: u64,
    gap: bool,
}
impl Cursor {
    fn load(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let mut fields = text.split_whitespace();
        let id = fields
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or(io::ErrorKind::InvalidData)?;
        Ok(Self {
            id,
            gap: fields.next() == Some("gap"),
        })
    }
    fn write(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_extension("tmp");
        let gap = if self.gap { " gap" } else { "" };
        fs::write(&temp, format!("{}{gap}", self.id))?;
        fs::rename(temp, path)
    }
}

pub struct History {
    cursor_file: PathBuf,
    boundary: Cursor,
    receiver: Option<Receiver<(Changes, bool)>>,
    original_records: usize,
    settled: bool,
}

impl History {
    pub fn start(index: &CompactIndex, repaint: eframe::egui::Context) -> Option<Self> {
        if index.is_empty() {
            return None;
        }
        let mut history = Self::after_rebuild(index)?;
        let since = match Cursor::load(&history.cursor_file) {
            Ok(cursor) => cursor,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(history),
            Err(_) => Cursor {
                id: u64::MAX,
                gap: true,
            },
        };
        history.boundary.gap = since.gap;
        if since.id != history.boundary.id {
            let roots = index.roots().to_vec();
            let (sender, receiver) = mpsc::channel();
            history.receiver = Some(receiver);
            history.settled = false;
            std::thread::spawn(move || {
                let names: Vec<_> = roots
                    .iter()
                    .filter_map(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok())
                    .collect();
                let pointers: Vec<_> = names.iter().map(|p| p.as_ptr()).collect();
                let mut changes = Changes {
                    roots,
                    ..Default::default()
                };
                let complete = unsafe {
                    miao_replay_history(
                        since.id,
                        pointers.as_ptr(),
                        pointers.len(),
                        collect,
                        (&mut changes as *mut Changes).cast(),
                    )
                };
                let _ = sender.send((changes, complete));
                repaint.request_repaint();
            });
        }
        Some(history)
    }

    pub fn has_gap(&self) -> bool {
        self.boundary.gap
    }

    pub fn poll(&mut self, index: &mut CompactIndex) -> Option<Result<bool, String>> {
        let (changes, complete) = match self.receiver.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => (Changes::default(), false),
        };
        self.receiver = None;
        let paths = index::dedup_paths(changes.structural.into_iter().collect());
        if !paths.is_empty()
            && let Err(e) = index::write_journal(
                &self.cursor_file.with_file_name(index::JOURNAL_NAME),
                &paths,
            )
        {
            return Some(Err(e));
        }
        index.apply_historical_dates(self.original_records, &changes.dates);
        self.settled = true;
        if !complete {
            // The missed interval requires a rebuild. Persist both the warning and a
            // new boundary now, even if this launch never gets a graceful shutdown.
            self.boundary.gap = true;
            if let Err(e) = self.checkpoint() {
                return Some(Err(e.to_string()));
            }
        }
        Some(Ok(!paths.is_empty()))
    }

    pub fn after_rebuild(index: &CompactIndex) -> Option<Self> {
        Some(Self {
            cursor_file: index::compact_index_path()
                .ok()?
                .with_file_name("dates-fsevents-v1"),
            boundary: Cursor {
                id: unsafe { FSEventsGetCurrentEventId() },
                gap: false,
            },
            receiver: None,
            settled: true,
            original_records: index.record_count(),
        })
    }

    pub fn checkpoint(&self) -> io::Result<()> {
        if self.settled {
            self.boundary.write(&self.cursor_file)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cutoff_advances_old_cursor_and_keeps_gap_until_rebuild() {
        let dir = std::env::temp_dir().join(format!("suishou-cursor-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cursor_file = dir.join("dates-fsevents-v1");
        fs::write(&cursor_file, "173566105").unwrap();
        assert!(!Cursor::load(&cursor_file).unwrap().gap);
        let (tx, rx) = mpsc::channel();
        let mut history = History {
            cursor_file: cursor_file.clone(),
            boundary: Cursor {
                id: 281380420,
                gap: false,
            },
            receiver: Some(rx),
            original_records: 0,
            settled: false,
        };
        history.checkpoint().unwrap();
        assert_eq!(Cursor::load(&cursor_file).unwrap().id, 173566105);
        let changes = Changes {
            structural: HashSet::from([dir.join("books/test.epub")]),
            ..Default::default()
        };
        tx.send((changes, false)).unwrap();
        assert!(history.poll(&mut CompactIndex::default()).unwrap().unwrap());
        assert!(dir.join(index::JOURNAL_NAME).metadata().unwrap().len() > 0);
        let persisted = Cursor::load(&cursor_file).unwrap();
        assert_eq!(persisted.id, 281380420);
        assert!(persisted.gap);
        let (tx, rx) = mpsc::channel();
        history.receiver = Some(rx);
        history.boundary.id += 100;
        tx.send((Changes::default(), true)).unwrap();
        history.poll(&mut CompactIndex::default()).unwrap().unwrap();
        // Successful replay advances only after its matching snapshot is durable.
        assert_eq!(Cursor::load(&cursor_file).unwrap().id, 281380420);
        history.checkpoint().unwrap();
        assert!(Cursor::load(&cursor_file).unwrap().gap);
        history.boundary.gap = false;
        history.checkpoint().unwrap();
        assert!(!Cursor::load(&cursor_file).unwrap().gap);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn historical_structural_events_survive_metadata_duplicates_and_skip_noise() {
        let mut changes = Changes {
            roots: vec!["/Users/sample".into()],
            ..Default::default()
        };
        let state = (&mut changes as *mut Changes).cast();
        let file = c"/Users/sample/orca/offline.epub";
        assert!(collect(state, file.as_ptr(), false));
        assert!(collect(state, file.as_ptr(), true));
        assert!(collect(
            state,
            c"/Users/sample/Library/Caches/noisy".as_ptr(),
            true
        ));
        assert_eq!(changes.dates.len(), 1);
        assert_eq!(
            changes.structural,
            HashSet::from([PathBuf::from(file.to_str().unwrap())])
        );
    }
}
