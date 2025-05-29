//! An in-memory log storage, with a fixed size for records.
#![allow(dead_code)]

use alloc::{boxed::Box, collections::vec_deque::VecDeque, string::String};
use core::{cell::RefCell, fmt::Display};
use embassy_time::Instant;

const DISCARD_ERROR: &str = "log discarded: too large for storage";

#[derive(Clone, Copy)]
pub struct SharedLogger {
    inner: &'static RefCell<LogStorage>,
}

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
}

#[derive(Clone, Debug)]
pub struct Record {
    pub instant: Instant,
    pub level: Level,
    pub text: String,
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
            Level::Trace => write!(f, "{}", "TRCE"),
            Level::Debug => write!(f, "{}", "DEBG"),
            Level::Info => write!(f, "{}", "INFO"),
            Level::Warn => write!(f, "{}", "WARN"),
            Level::Error => write!(f, "{}", "ERRO"),
        }
    }
}

impl LogStorage {
    const fn with_capacity(capacity: usize) -> Self {
        LogStorage {
            records: VecDeque::new(),
            utilization: 0,
            capacity,
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

        // Store the new record.
        self.utilization += text.len();
        self.records.push_front(Record {
            instant: Instant::now(),
            level,
            text,
        });
    }

    fn clear(&mut self) {
        self.utilization = 0;
        self.records.clear();
    }
}

impl SharedLogger {
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
