//! An in-memory log storage, with a fixed size for records.
#![allow(dead_code)]

use alloc::{boxed::Box, collections::vec_deque::VecDeque, format, string::String};
use core::{cell::RefCell, fmt::Display};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, watch};
use embassy_time::Instant;

const MEMLOG_WATCHERS: usize = 2;
const DISCARD_ERROR: &str = "log discarded: too large for storage";

#[derive(Clone, Copy)]
pub struct SharedLogger {
    inner: &'static RefCell<LogStorage>,
}

pub type LogDynReceiver = watch::DynReceiver<'static, Record>;

pub fn init(capacity: usize) -> SharedLogger {
    // Ensure we have enough space to store the error about not having enough space.
    if capacity < DISCARD_ERROR.len() {
        panic!("minimum log storage capacity is {}", DISCARD_ERROR.len());
    }

    let storage = LogStorage::with_capacity(capacity);
    SharedLogger {
        inner: Box::leak(Box::new(RefCell::new(storage))),
    }
}

struct LogStorage {
    records: VecDeque<Record>,
    // In characters.
    utilization: usize,
    capacity: usize,
    // If enabled, prints new records over esp_println.
    print: bool,
    // If set, broadcasts new records over the watch channel.
    watch: Option<&'static watch::Watch<NoopRawMutex, Record, MEMLOG_WATCHERS>>,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub instant: Instant,
    pub level: Level,
    pub text: String,
}

impl Display for Record {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let timestamp = format_milliseconds_to_hms(self.instant.as_millis());
        write!(f, "[{}] {}: {}", timestamp, self.level, self.text)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Display for Level {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Level::Trace => write!(f, "TRCE"),
            Level::Debug => write!(f, "DEBG"),
            Level::Info => write!(f, "INFO"),
            Level::Warn => write!(f, "WARN"),
            Level::Error => write!(f, "ERRO"),
        }
    }
}

impl LogStorage {
    fn with_capacity(capacity: usize) -> Self {
        LogStorage {
            records: VecDeque::new(),
            utilization: 0,
            capacity,
            print: false,
            watch: None,
        }
    }

    fn add_record(&mut self, level: Level, text: impl Into<String>) {
        let text: String = text.into();

        // Can't fit this record in storage. Log a warning.
        if text.len() > self.capacity {
            self.add_record(Level::Warn, DISCARD_ERROR);
            return;
        }

        // At this point we know we have enough capacity (even if all existing
        // records need to be removed), so we can safely use unwraps.

        // Pop existing records until we have enough space for the new record.
        while (self.capacity - self.utilization) < text.len() {
            let removed = self.records.pop_back().unwrap();
            self.utilization -= removed.text.len();
        }

        self.utilization += text.len();

        let new_record = Record {
            instant: Instant::now(),
            level,
            text,
        };

        // If log printing is enabled, print this record.
        if self.print {
            esp_println::println!("{new_record}");
        }

        // If log watching is enabled, share this record.
        if let Some(watch) = self.watch {
            watch.sender().send(new_record.clone());
        }

        // Store the new record.
        self.records.push_front(new_record);
    }

    fn clear(&mut self) {
        self.utilization = 0;
        self.records.clear();
    }
}

impl SharedLogger {
    pub fn enable_print(&self) {
        self.inner.borrow_mut().print = true;
    }

    pub fn enable_watch(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.watch.is_none() {
            inner.watch = Some(Box::leak(Box::new(watch::Watch::new())));
        }
    }

    // Get a watcher to be notified of new logs.
    //
    // Returns None if log watching is not enabled, or if the number of watchers is exhausted.
    pub fn watch(&self) -> Option<LogDynReceiver> {
        self.inner
            .borrow()
            .watch
            .map(|watch| watch.dyn_receiver())?
    }

    pub fn trace(&self, text: impl Into<String>) {
        self.inner.borrow_mut().add_record(Level::Trace, text);
    }
    pub fn debug(&self, text: impl Into<String>) {
        self.inner.borrow_mut().add_record(Level::Debug, text);
    }
    pub fn info(&self, text: impl Into<String>) {
        self.inner.borrow_mut().add_record(Level::Info, text);
    }
    pub fn warn(&self, text: impl Into<String>) {
        self.inner.borrow_mut().add_record(Level::Warn, text);
    }
    pub fn error(&self, text: impl Into<String>) {
        self.inner.borrow_mut().add_record(Level::Error, text);
    }
    pub fn clear(&self) {
        self.inner.borrow_mut().clear();
    }
    pub fn records(&self) -> core::cell::Ref<'_, VecDeque<Record>> {
        core::cell::Ref::map(self.inner.borrow(), |storage| &storage.records)
    }
}

/// Formats a u64 millisecond value into "HHHHH:MM:SS.xxx" string.
#[inline]
pub fn format_milliseconds_to_hms(total_ms: u64) -> String {
    let millis_part = total_ms % 1000;
    let total_seconds = total_ms / 1000;

    let seconds_part = total_seconds % 60;
    let total_minutes = total_seconds / 60;

    let minutes_part = total_minutes % 60;
    let hours_part = total_minutes / 60;

    format!(
        "{:05}:{:02}:{:02}.{:03}",
        hours_part, minutes_part, seconds_part, millis_part
    )
}
