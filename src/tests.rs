use crate::Arc;

#[cfg(feature = "loom")]
use loom::{model, sync, thread};

#[cfg(not(feature = "loom"))]
use std::{sync, thread};

#[cfg(not(feature = "loom"))]
pub fn model<F: Fn() + Sync + Send + 'static>(f: F) {
    const ROUNDS: usize = 1_000;
    for _ in 0..ROUNDS {
        f();
    }
}

#[derive(Debug, Default, Clone)]
struct DropMonitor(sync::Arc<()>);

impl DropMonitor {
    fn is_unique(&self) -> bool {
        sync::Arc::strong_count(&self.0) == 1
    }
}

fn new_monitored_arc() -> (DropMonitor, Arc<DropMonitor>) {
    let monitor = DropMonitor::default();
    let arc = Arc::new(monitor.clone());
    (monitor, arc)
}

#[test]
fn trivial_drop() {
    model(|| {
        let (monitor, v1) = new_monitored_arc();
        assert!(!monitor.is_unique());
        drop(v1);
        assert!(monitor.is_unique());
    });
}

#[test]
fn trivial_clone() {
    model(|| {
        let (monitor, v1) = new_monitored_arc();
        let v2 = v1.clone();
        let v3 = v2.clone();
        drop(v1);
        drop(v2);
        assert!(!monitor.is_unique());
        drop(v3);
        assert!(monitor.is_unique());
    });
}

#[test]
fn trivial_upgrade() {
    model(|| {
        let (monitor, v1) = new_monitored_arc();
        let w1 = Arc::downgrade(&v1);
        drop(w1.upgrade());
        assert!(!monitor.is_unique());
        drop(v1);
        assert!(monitor.is_unique());
        drop(w1);
    });
}

#[test]
fn clone_clone() {
    model(|| {
        let (monitor, v1) = new_monitored_arc();
        let threads = (0..2)
            .map(|_| {
                let v2 = v1.clone();
                thread::spawn(move || {
                    let v3 = v2.clone();
                    drop(v2);
                    drop(v3);
                })
            })
            .collect::<Vec<_>>();
        assert!(!monitor.is_unique());
        drop(v1);
        threads.into_iter().for_each(|j| j.join().unwrap());
        assert!(monitor.is_unique());
    });
}

#[test]
fn clone_drop_upgrade() {
    model(|| {
        let (monitor, v) = new_monitored_arc();
        let mut threads = Vec::new();

        // Clone strong.
        threads.push(thread::spawn({
            let v2 = v.clone();
            move || {
                let v3 = v2.clone();
                drop(v2);
                drop(v3);
            }
        }));
        // Upgrade weak.
        threads.push(thread::spawn({
            let w = Arc::downgrade(&v);
            move || {
                let upgraded = w.upgrade();
                drop(upgraded);
            }
        }));
        assert!(!monitor.is_unique());
        // Drop strong.
        drop(v);
        threads.into_iter().for_each(|j| j.join().unwrap());
        assert!(monitor.is_unique());
    });
}

#[test]
#[cfg_attr(feature = "loom", ignore = "too slow under loom")]
fn upgrade_upgrade() {
    model(|| {
        let (monitor, v) = new_monitored_arc();
        let mut threads = Vec::new();
        // Upgrade weak.
        for _ in 0..2 {
            threads.push(thread::spawn({
                let w = Arc::downgrade(&v);
                move || {
                    let upgraded = w.upgrade();
                    drop(upgraded);
                }
            }));
        }
        // Drop strong.
        drop(v);
        threads.into_iter().for_each(|j| j.join().unwrap());
        assert!(monitor.is_unique());
    });
}
