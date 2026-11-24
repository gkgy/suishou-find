use eframe::egui;
use miao_find::index::SearchSession;
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};
pub const PAGE_SIZE: usize = 500;
const CACHE_PAGES: usize = 8;
enum Job {
    Reset(u64, SearchSession),
    Page(u64, usize),
}
struct Reply {
    generation: u64,
    page: usize,
    ids: Vec<usize>,
}
/// Full-length virtual list with a bounded cache of result windows. Only visible
/// rows (and the next boundary) are requested. No accumulating rendered list.
pub struct Paging {
    sender: SyncSender<Job>,
    receiver: Receiver<Reply>,
    generation: Arc<AtomicU64>,
    pending_reset: Option<Job>,
    pending_pages: VecDeque<usize>,
    in_flight: Vec<usize>,
    cache: VecDeque<(usize, Vec<usize>)>,
    pub search_key: String,
    pub total: usize,
    pub selection_row: usize,
    pending_selection: Option<usize>,
    pub reset_scroll: bool,
    pub visible: Range<usize>,
    last_query: Instant,
}
fn cache_put(cache: &mut VecDeque<(usize, Vec<usize>)>, page: usize, ids: Vec<usize>) {
    cache.retain(|(p, _)| *p != page);
    cache.push_back((page, ids));
    while cache.len() > CACHE_PAGES {
        cache.pop_front();
    }
}
impl Paging {
    pub fn new(context: egui::Context) -> Self {
        let (sender, jobs) = mpsc::sync_channel(1);
        let (responses, receiver) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = generation.clone();
        std::thread::spawn(move || {
            let mut snapshot: Option<(u64, SearchSession)> = None;
            let mut cache: VecDeque<(usize, Vec<usize>)> = VecDeque::new();
            while let Ok(job) = jobs.recv() {
                match job {
                    Job::Reset(g, session) => {
                        if current.load(Ordering::Acquire) == g {
                            snapshot = Some((g, session));
                            cache.clear();
                        }
                    }
                    Job::Page(g, page) => {
                        if current.load(Ordering::Acquire) != g {
                            continue;
                        }
                        let Some((sg, session)) = snapshot.as_mut() else {
                            continue;
                        };
                        if *sg != g {
                            continue;
                        }
                        let ids = if let Some((_, ids)) = cache.iter().find(|(p, _)| *p == page) {
                            ids.clone()
                        } else {
                            let ids = session.page(page, PAGE_SIZE);
                            cache_put(&mut cache, page, ids.clone());
                            ids
                        };
                        if current.load(Ordering::Acquire) == g {
                            if responses
                                .send(Reply {
                                    generation: g,
                                    page,
                                    ids,
                                })
                                .is_err()
                            {
                                break;
                            }
                            context.request_repaint();
                        }
                    }
                }
            }
        });
        Self {
            sender,
            receiver,
            generation,
            pending_reset: None,
            pending_pages: VecDeque::new(),
            in_flight: Vec::new(),
            cache: VecDeque::new(),
            search_key: String::new(),
            total: 0,
            selection_row: 0,
            pending_selection: None,
            reset_scroll: false,
            visible: 0..0,
            last_query: Instant::now(),
        }
    }

    pub fn refreshed_page(&self, key: &str, total: usize) -> usize {
        if self.search_key == key {
            self.selection_row.min(total.saturating_sub(1)) / PAGE_SIZE
        } else {
            0
        }
    }
    pub fn reset(&mut self, session: SearchSession, first: &[usize], page: usize, key: String) {
        let g = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.total = session.total();
        self.reset_scroll = self.search_key != key;
        if self.reset_scroll {
            self.selection_row = 0;
            self.visible = 0..0;
        }
        self.selection_row = self.selection_row.min(self.total.saturating_sub(1));
        self.search_key = key;
        self.pending_selection = None;
        self.cache.clear();
        cache_put(&mut self.cache, page, first.to_vec());
        self.pending_pages.clear();
        self.in_flight.clear();
        self.pending_reset = Some(Job::Reset(g, session));
        self.last_query = Instant::now();
        self.flush();
    }
    fn flush(&mut self) {
        if let Some(job) = self.pending_reset.take() {
            match self.sender.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    self.pending_reset = Some(job);
                    return;
                }
                Err(TrySendError::Disconnected(_)) => return,
            }
        }
        while let Some(page) = self.pending_pages.pop_front() {
            match self
                .sender
                .try_send(Job::Page(self.generation.load(Ordering::Acquire), page))
            {
                Ok(()) => self.in_flight.push(page),
                Err(TrySendError::Full(_)) => {
                    self.pending_pages.push_front(page);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    }
    pub fn id(&self, row: usize) -> Option<usize> {
        if row >= self.total {
            return None;
        }
        self.cache
            .iter()
            .find(|(p, _)| *p == row / PAGE_SIZE)
            .and_then(|(_, ids)| ids.get(row % PAGE_SIZE))
            .copied()
    }
    fn queue(&mut self, page: usize) {
        if page >= self.total.div_ceil(PAGE_SIZE)
            || self.cache.iter().any(|(p, _)| *p == page)
            || self.pending_pages.contains(&page)
            || self.in_flight.contains(&page)
        {
            return;
        }
        self.pending_pages.push_back(page);
    }
    pub fn show_range(&mut self, range: Range<usize>, context: &egui::Context) {
        self.visible = range.start.min(self.total)..range.end.min(self.total);
        // Drop obsolete queued viewport requests before sending current ones.
        self.pending_pages.clear();
        if let Some(row) = self.pending_selection {
            self.queue(row / PAGE_SIZE);
        }
        if !self.visible.is_empty() {
            for page in self.visible.start / PAGE_SIZE..=(self.visible.end - 1) / PAGE_SIZE {
                self.queue(page);
            }
            // A small look-ahead crosses only the next boundary, after typing settles.
            let page = (self.visible.end - 1) / PAGE_SIZE;
            if self.last_query.elapsed().as_millis() >= 150
                && self.visible.end % PAGE_SIZE >= PAGE_SIZE - 64
            {
                self.queue(page + 1);
            }
        }
        self.flush();
        if !self.pending_pages.is_empty() || self.visible_loading() {
            context.request_repaint_after(std::time::Duration::from_millis(16));
        } else if self.last_query.elapsed().as_millis() < 150 {
            context.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
    pub fn visible_loading(&self) -> bool {
        !self.visible.is_empty()
            && (self.visible.start / PAGE_SIZE..=(self.visible.end - 1) / PAGE_SIZE)
                .any(|page| !self.cache.iter().any(|(p, _)| *p == page))
    }
    pub fn select(&mut self, row: usize, selected: &mut Option<usize>) {
        if self.total == 0 {
            *selected = None;
            return;
        }
        self.selection_row = row.min(self.total - 1);
        *selected = self.id(self.selection_row);
        self.pending_selection = selected.is_none().then_some(self.selection_row);
        if self.pending_selection.is_some() {
            self.queue(self.selection_row / PAGE_SIZE);
            self.flush();
        }
    }
    pub fn poll(&mut self, selected: &mut Option<usize>, context: &egui::Context) {
        self.flush();
        while let Ok(reply) = self.receiver.try_recv() {
            if reply.generation != self.generation.load(Ordering::Acquire) {
                continue;
            }
            self.in_flight.retain(|p| *p != reply.page);
            cache_put(&mut self.cache, reply.page, reply.ids);
            if let Some(row) = self.pending_selection {
                if let Some(id) = self.id(row) {
                    *selected = Some(id);
                    self.pending_selection = None;
                }
            }
        }
        self.flush();
        if self.pending_reset.is_some()
            || !self.pending_pages.is_empty()
            || self.pending_selection.is_some()
        {
            context.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty() -> SearchSession {
        SearchSession::new(
            &miao_find::index::CompactIndex::default(),
            "",
            miao_find::index::Category::All,
            miao_find::index::SortOrder::Name,
            false,
            None,
            None,
            None,
            false,
        )
    }
    #[test]
    fn virtual_rows_cross_500_without_accumulating_results_and_keyboard_can_cross_boundary() {
        let context = egui::Context::default();
        let mut p = Paging::new(context.clone());
        p.total = 1_500_000;
        cache_put(&mut p.cache, 0, (0..500).collect());
        assert_eq!(p.id(499), Some(499));
        assert_eq!(p.id(500), None);
        assert_eq!(p.cache.len(), 1);
        p.show_range(490..510, &context);
        assert!(p.visible_loading());
        let (tx, rx) = mpsc::channel();
        p.receiver = rx;
        let mut selected = Some(499);
        p.select(500, &mut selected);
        assert_eq!(selected, None);
        tx.send(Reply {
            generation: 0,
            page: 1,
            ids: (500..1000).collect(),
        })
        .unwrap();
        p.poll(&mut selected, &context);
        assert_eq!(selected, Some(500));
        assert_eq!(p.id(500), Some(500));
        assert!(!p.visible_loading());
        assert_eq!(p.cache.len(), 2);
        assert!(!p.reset_scroll);
        // Jumping far away requests that window, without building preceding windows.
        p.show_range(1_000_100..1_000_120, &context);
        assert_eq!(p.id(1_000_100), None);
        assert_eq!(p.cache.len(), 2);
        assert_eq!(p.visible.start, 1_000_100);
    }
    #[test]
    fn stale_replies_cannot_overwrite_new_query_and_refresh_preserves_viewport() {
        let context = egui::Context::default();
        let mut p = Paging::new(context.clone());
        p.total = 1200;
        p.search_key = "same".into();
        p.selection_row = 750;
        p.visible = 700..720;
        assert_eq!(p.refreshed_page("same", 1200), 1);
        assert_eq!(p.refreshed_page("new", 1200), 0);
        p.reset(empty(), &[], 0, "new".into());
        assert_eq!(p.visible, 0..0);
        assert!(p.reset_scroll);
        let (tx, rx) = mpsc::channel();
        p.receiver = rx;
        let mut selected = None;
        tx.send(Reply {
            generation: 0,
            page: 0,
            ids: vec![999],
        })
        .unwrap();
        p.poll(&mut selected, &context);
        assert_eq!(p.id(0), None);
        assert_eq!(selected, None);
        p.visible = 500..520;
        p.selection_row = 500;
        p.reset(empty(), &[], 0, "new".into());
        assert_eq!(p.visible, 500..520);
        assert!(!p.reset_scroll);
    }
    #[test]
    fn cache_remains_bounded_even_after_scrolling_through_many_windows() {
        let mut cache = VecDeque::new();
        for page in 0..10_000 {
            cache_put(&mut cache, page, (0..500).collect());
        }
        assert_eq!(cache.len(), CACHE_PAGES);
        assert_eq!(cache.iter().map(|(_, ids)| ids.len()).sum::<usize>(), 4000);
    }
}
