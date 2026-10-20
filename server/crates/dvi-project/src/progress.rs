//! Progress of a project configuration and build, read by the thread that reports it, and the
//! request to cancel it.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// What a build is doing. `done` and `total` count files, except while scanning, where `done`
/// counts the sources found and the total is not known yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Starting,
    /// Auto-configuration: looking for sources in the project folder.
    Scanning,
    /// Auto-configuration: reading each source for its includes, packages and macros.
    Examining,
    Reading,
    Preprocessing,
    Parsing,
    Indexing,
}

const PHASES: [Phase; 7] =
    [Phase::Starting, Phase::Scanning, Phase::Examining, Phase::Reading, Phase::Preprocessing, Phase::Parsing, Phase::Indexing];

#[derive(Default)]
pub struct Progress {
    phase: AtomicU8,
    done: AtomicUsize,
    total: AtomicUsize,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Progress {
        Progress::default()
    }

    /// Starts a phase of `total` files (0 when not known).
    pub fn begin(&self, phase: Phase, total: usize) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    /// One more file done (or found, while scanning).
    pub fn step(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (Phase, usize, usize) {
        let phase = PHASES.get(self.phase.load(Ordering::Relaxed) as usize).copied().unwrap_or(Phase::Starting);
        (phase, self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// A line for people, e.g. "parsing 1,140 / 4,000 files".
    pub fn message(&self) -> String {
        let (phase, done, total) = self.snapshot();
        let of = |what: &str| format!("{what} {} / {} files", thousands(done.min(total)), thousands(total));
        match phase {
            Phase::Starting => "starting".into(),
            Phase::Scanning => format!("looking for sources: {} found", thousands(done)),
            Phase::Examining => of("examining"),
            Phase::Reading => of("reading"),
            Phase::Preprocessing => of("preprocessing"),
            Phase::Parsing => of("parsing"),
            Phase::Indexing => "indexing".into(),
        }
    }

    /// Rough share of the whole configuration and build that is done, 0 to 100. Each phase
    /// gets a slice; scanning, whose total is unknown, stays at its start.
    pub fn percentage(&self) -> u32 {
        let (phase, done, total) = self.snapshot();
        let (from, to) = match phase {
            Phase::Starting | Phase::Scanning => (0, 0),
            Phase::Examining => (0, 15),
            Phase::Reading => (15, 30),
            Phase::Preprocessing => (30, 65),
            Phase::Parsing => (65, 95),
            Phase::Indexing => (95, 99),
        };
        let part = if total == 0 { 0 } else { (to - from) * done.min(total) / total };
        (from + part) as u32
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_percentages() {
        let p = Progress::new();
        assert_eq!(p.message(), "starting");
        p.begin(Phase::Scanning, 0);
        for _ in 0..3200 {
            p.step();
        }
        assert_eq!(p.message(), "looking for sources: 3,200 found");
        assert_eq!(p.percentage(), 0);
        p.begin(Phase::Parsing, 4000);
        for _ in 0..2000 {
            p.step();
        }
        assert_eq!(p.message(), "parsing 2,000 / 4,000 files");
        assert_eq!(p.percentage(), 80);
        assert!(!p.is_cancelled());
        p.cancel();
        assert!(p.is_cancelled());
    }
}
