// Check the dates of existing records changed between launches, using the
// host FSEvents journal. The GUI can search the cached dates during replay.
use std::{
    collections::HashMap,
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
        sink: extern "C" fn(*mut c_void, *const c_char) -> bool,
        state: *mut c_void,
    ) -> bool;
}

type Updates = HashMap<u64, Option<i64>>;

extern "C" fn collect(state: *mut c_void, path: *const c_char) -> bool {
    // C calls this synchronously on the worker thread with a NUL-terminated path.
    let updates = unsafe { &mut *state.cast::<Updates>() };
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else {
        return true;
    };
    let hash = index::hash_path(path);
    if !updates.contains_key(&hash) {
        // Keep a gap or an unusually large offline backlog from consuming
        // unbounded memory. A manual rebuild can reconcile an expired journal.
        if updates.len() >= 250_000 {
            return false;
        }
        updates.insert(hash, index::file_date(Path::new(path)));
    }
    true
}

pub struct History {
    cursor_file: PathBuf,
    start: u64,
    receiver: Option<Receiver<(Updates, bool)>>,
    complete: bool,
    original_records: usize,
}

impl History {
    pub fn start(index: &CompactIndex, repaint: eframe::egui::Context) -> Option<Self> {
        if index.is_empty() {
            return None;
        }
        let cursor_file = index::compact_index_path()
            .ok()?
            .with_file_name("dates-fsevents-v1");
        let start = unsafe { FSEventsGetCurrentEventId() };
        let since = match fs::read_to_string(&cursor_file) {
            Ok(text) => Some(text.trim().parse::<u64>().unwrap_or(u64::MAX)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => Some(u64::MAX),
        };
        let mut history = Self {
            cursor_file,
            start,
            receiver: None,
            complete: true,
            original_records: index.record_count(),
        };
        if let Some(since) = since.filter(|id| *id != start) {
            let roots = index.roots().to_vec();
            let (sender, receiver) = mpsc::channel();
            history.receiver = Some(receiver);
            history.complete = false;
            std::thread::spawn(move || {
                let names: Vec<_> = roots
                    .iter()
                    .filter_map(|p| CString::new(p.as_os_str().as_encoded_bytes()).ok())
                    .collect();
                let pointers: Vec<_> = names.iter().map(|p| p.as_ptr()).collect();
                let mut updates = Updates::new();
                let complete = unsafe {
                    miao_replay_history(
                        since,
                        pointers.as_ptr(),
                        pointers.len(),
                        collect,
                        (&mut updates as *mut Updates).cast(),
                    )
                };
                let _ = sender.send((updates, complete));
                repaint.request_repaint();
            });
        }
        Some(history)
    }

    pub fn poll(&mut self, index: &mut CompactIndex) -> Option<bool> {
        let result = self.receiver.as_ref()?.try_recv();
        match result {
            Ok((updates, complete)) => {
                index.apply_historical_dates(self.original_records, &updates);
                self.complete = complete;
            }
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => self.complete = false,
        }
        self.receiver = None;
        Some(self.complete)
    }

    pub fn after_rebuild(index: &CompactIndex) -> Option<Self> {
        Some(Self {
            cursor_file: index::compact_index_path()
                .ok()?
                .with_file_name("dates-fsevents-v1"),
            start: unsafe { FSEventsGetCurrentEventId() },
            receiver: None,
            complete: true,
            original_records: index.record_count(),
        })
    }

    pub fn checkpoint(&self) -> io::Result<()> {
        if self.complete {
            // Save the launch boundary only after the paired index and dates
            // are on disk. An interrupted session can safely replay events again.
            let temp = self.cursor_file.with_extension("tmp");
            fs::write(&temp, self.start.to_string())?;
            fs::rename(temp, &self.cursor_file)?;
        }
        Ok(())
    }
}
