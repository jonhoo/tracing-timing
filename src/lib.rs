//! Inter-event timing metrics.
//!
//! This crate provides a `tracing::Subscriber` that keeps statistics on inter-event timing
//! information. More concretely, given code like this:
//!
//! ```rust
//! use tokio_trace::*;
//! use tracing_metrics::{Builder, Histogram};
//! let subscriber = Builder::from(|| Histogram::new_with_max(1_000_000, 2).unwrap()).build();
//! let dispatcher = Dispatch::new(subscriber);
//! dispatcher::with_default(&dispatcher, || {
//!     trace_span!("request").in_scope(|| {
//!         // do a little bit of work
//!         trace!("fast");
//!         // do a lot of work
//!         trace!("slow");
//!     })
//! });
//! eprintln!("FOO");
//! ```
//!
//! You can produce something like this:
//!
//! ```text
//! fast:
//!   50µs |
//!  100µs |
//!  150µs | ****
//!  200µs | ************
//!  250µs | *************
//!  300µs | *****
//!  350µs | ****
//!  400µs | *
//!  450µs |
//!  500µs |
//!
//! slow:
//!  550µs |
//!  600µs |
//!  650µs | ******
//!  700µs | **********************
//!  750µs | *********
//!  800µs | **
//!  850µs | *
//!  900µs |
//! ```
//!
//! When [`Sampler`] is used as the `tracing::Dispatch`, the time between each event in a span is
//! measured using [`quanta`], and is recorded in "[high dynamic range histograms]" using
//! [`hdrhistogram`]'s multi-threaded recording facilities. The recorded metrics are grouped using
//! the [`SpanGroup`] and [`EventGroup`] traits, allowing you to combine recorded statistics across
//! spans and events.
//!
//! ## Extracting metrics
//!
//! The crate does not implement a mechanism for recording the resulting histograms. Instead, you
//! can implement this as you see fit using [`Sampler::with_histograms`]. It gives you access to
//! the histograms for all groups. Note that you must call `refresh()` on each histogram to see its
//! latest values (see [`hdrhistogram::SyncHistogram`]).
//!
//! To access the histograms, you (currently) need to use `tracing::Dispatch::downcast_ref`.
//! The mystical black magic invocations you need to issue are as follows:
//!
//! ```rust
//! use tokio_trace::*;
//! use tracing_metrics::{Builder, Histogram};
//! let subscriber = Builder::from(|| Histogram::new_with_max(1_000_000, 2).unwrap()).build();
//! // magic #1:
//! let mut _type_of_subscriber = if false { Some(&subscriber) } else { None };
//! let dispatch = Dispatch::new(subscriber);
//! // ...
//! // code that hands off clones of the dispatch
//! // maybe to other threads
//! // ...
//! _type_of_subscriber = dispatch.downcast_ref();
//! _type_of_subscriber.unwrap().with_histograms(|hs| {
//!     for (span_group, hs) in hs {
//!         for (event_group, h) in hs {
//!             // make sure we see the latest samples:
//!             h.refresh();
//!             // print the median:
//!             println!("{} -> {}: {}ns", span_group, event_group, h.value_at_quantile(0.5))
//!         }
//!     }
//! });
//! ```
//!
//! See the documentation for [`hdrhistogram`] for more on what you can do once you have the
//! histograms.
//!
//! ## Grouping samples
//!
//! By default, [`Sampler`] groups samples by the "name" of the containing span and the "message"
//! of the relevant event. These are the first string parameter you pass to each of the relevant
//! tracing macros. You can override this behavior either by providing your own implementation of
//! [`SpanGroup`] and [`EventGroup`] to [`Builder::spans`] and [`Builder::events`] respectively.
//! There are also a number of pre-defined "groupers" in the [`group`] module that cover the most
//! common cases.
//!
//!   [high dynamic range histograms]: https://hdrhistogram.github.io/HdrHistogram/
//!   [`hdrhistogram`]: https://docs.rs/hdrhistogram/
//!   [`quanta`]: https://docs.rs/quanta/
//!   [`hdrhistogram::SyncHistogram`]: https://docs.rs/hdrhistogram/6/hdrhistogram/sync/struct.SyncHistogram.html
//!

#![deny(missing_docs)]

use crossbeam::sync::ShardedLock;
use fnv::FnvHashMap as HashMap;
use hdrhistogram::{sync::Recorder, SyncHistogram};
use slab::Slab;
use std::cell::{RefCell, UnsafeCell};
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{atomic, Mutex};
use tokio_trace_core::*;

thread_local! {
    static SPAN: RefCell<Option<span::Id>> = RefCell::new(None);
    static TID: atomic::AtomicUsize = atomic::AtomicUsize::new(0);
    static MYTID: RefCell<Option<usize>> = RefCell::new(None);
}

mod builder;
pub use builder::Builder;
pub use hdrhistogram::Histogram;

pub mod group;

type Map<S, E, T> = HashMap<S, HashMap<E, T>>;

/// Translate attributes from a tracing span into a metrics span group.
///
/// All spans whose attributes produce the same `Id`-typed value when passed through `group`
/// share a namespace for the groups produced by [`EventGroup::group`] on their contained events.
pub trait SpanGroup {
    /// The type of the metrics span group.
    type Id;

    /// Extract the group for this span's attributes.
    fn group(&self, span: &span::Attributes) -> Self::Id;
}

/// Translate attributes from a tracing event into a metrics event group.
///
/// All events that share a [`SpanGroup`], and whose attributes produce the same `Id`-typed value
/// when passed through `group`, are considered a single metrics target, and have their samples
/// recorded together.
pub trait EventGroup {
    /// The type of the metrics event group.
    type Id;

    /// Extract the group for this event.
    fn group(&self, event: &Event) -> Self::Id;
}

struct SamplerInner<S, E> {
    // We need fast access to the last event for each span.
    last_event: Slab<atomic::AtomicU64>,

    // how many references are there to each span id?
    // needed so we know when to reclaim
    refcount: Slab<atomic::AtomicUsize>,

    // note that many span::Ids can map to the same S
    spans: HashMap<span::Id, S>,

    // (S + callsite) => E => TID => Recorder
    recorders: Map<S, E, ThreadLocalRecorder>,

    // used to produce a Recorder for a thread that has not recorded for a given sid/eid pair
    idle_recorders: Map<S, E, hdrhistogram::sync::IdleRecorder<Recorder<u64>, u64>>,
}

impl<S, E> Default for SamplerInner<S, E>
where
    S: Eq + Hash,
{
    fn default() -> Self {
        SamplerInner {
            last_event: Default::default(),
            refcount: Default::default(),
            spans: Default::default(),
            recorders: Default::default(),
            idle_recorders: Default::default(),
        }
    }
}

struct MasterHistograms<NH, S, E> {
    new_histogram: NH,
    histograms: HashMap<S, HashMap<E, SyncHistogram<u64>>>,
}

impl<NH, S, E> From<NH> for MasterHistograms<NH, S, E>
where
    S: Hash + Eq,
    E: Hash + Eq,
{
    fn from(nh: NH) -> Self {
        MasterHistograms {
            new_histogram: nh,
            histograms: Default::default(),
        }
    }
}

impl<NH, S, E> MasterHistograms<NH, S, E>
where
    NH: FnMut() -> Histogram<u64>,
    S: Hash + Eq,
    E: Hash + Eq,
{
    fn make(&mut self, span_group: S, event_group: E) -> Recorder<u64> {
        let nh = &mut self.new_histogram;
        self.histograms
            .entry(span_group)
            .or_insert_with(HashMap::default)
            .entry(event_group)
            .or_insert_with(move || (nh)().into_sync())
            .recorder()
    }
}

/// Metrics-gathering tracing subscriber.
///
/// This type is constructed using a [`Builder`].
///
/// See the [crate-level docs] for details.
///
///   [crate-level docs]: ../
pub struct Sampler<NH, S = group::ByName, E = group::ByMessage>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    span_group: S,
    event_group: E,
    time: quanta::Clock,

    shared: ShardedLock<SamplerInner<S::Id, E::Id>>,
    histograms: Mutex<MasterHistograms<NH, S::Id, E::Id>>,
}

impl<NH, S, E> Sampler<NH, S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
    NH: FnMut() -> Histogram<u64> + Send,
{
    fn time_event(&self, span: &span::Id) -> Option<u64> {
        let now = self.time.now();
        let previous = self.shared.read().unwrap().last_event[span.into_u64() as usize - 1]
            .swap(now, atomic::Ordering::AcqRel);
        if previous > now {
            // someone else recorded a sample _just_ now
            // the delta is effectively zero, but recording a 0 sample is misleading
            None
        } else {
            Some(now - previous)
        }
    }

    fn with_recorder<F>(&self, span: &span::Id, event: &Event, f: F)
    where
        F: FnOnce(&mut Recorder<u64>),
    {
        // who are we?
        let tid = ThreadId::default();

        // fast path: sid/eid pair is known to this thread
        let eid = self.event_group.group(event);
        let inner = self.shared.read().unwrap();
        let tmp = &inner.recorders[&inner.spans[span]];
        if let Some(ref recorder) = tmp.get(&eid).and_then(|recorders| recorders.get(&tid)) {
            // we know no-one else has our TID
            f(unsafe { &mut *recorder.get() });
            return;
        }

        // slow path: either sid/eid pair was new, or it was new _to this thread_
        // in either case, we need to create entry for recorder where there was none

        // to avoid taking unnecessary locks later, let's short-cut if there is already an idle
        // recorder available for this sid/eid pair
        let recorder = inner.idle_recorders[&inner.spans[span]]
            .get(&eid)
            .map(|ir| ir.recorder())
            .ok_or_else(|| {
                // if there wasn't one, we need the span group for later
                inner.spans[span].clone()
            });

        // we're going to have to cache the recorder for next time
        // to do that, we need to take the write lock, so we must first drop the read lock
        drop(tmp);
        drop(inner);

        // we're going to need a recorder one way or another
        let mut recorder = recorder.unwrap_or_else(|sgi| {
            // there wasn't a recorder available for the sid/eid pair, so we need to make a new
            // histogram for that pair and gets its recorder.
            //
            // note that we take care to take this lock while not holding any other locks!
            //
            // note also that we _may_ end up with multiple subscribers all deciding to take this
            // lock if a new event group appears in multiple places at once. that will slow things
            // down a little, but should not deadlock.
            //
            // the biggest concern with taking the lock here is deadlocking with `with_histogram`
            // if the user decides to call `.refresh()` in there...
            self.histograms.lock().unwrap().make(sgi, eid.clone())
        });
        f(&mut recorder);

        // now time to cache the recorder for next time
        let mut inner = self.shared.write().unwrap();
        let inner = &mut *inner;

        let e = inner
            .recorders
            .get_mut(&inner.spans[span])
            .unwrap()
            .entry(eid)
            .or_insert_with(ThreadLocalRecorder::default)
            .entry(tid);

        if let Entry::Vacant(e) = e {
            e.insert(UnsafeCell::new(recorder));
        } else {
            // this should not be possible.
            // we entered the slow path because there was no entry for either our tid, or for the
            // whole eid (which would also imply our tid). since we are the only thread with our
            // tid, no-one else should have filled it for us.
            unreachable!();
        }
    }

    /// Access the metrics histograms.
    ///
    /// Be aware that the contained histograms are not automatically updated to reflect recently
    /// gathered samples. For each histogram you wish to read from, you must call `refresh` or
    /// `refresh_timeout` to gather up-to-date samples.
    ///
    /// For information about what you can do with the histograms, see the [`hdrhistogram`
    /// documentation].
    ///
    ///   [`hdrhistogram` documentation]: https://docs.rs/hdrhistogram/
    pub fn with_histograms<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut HashMap<S::Id, HashMap<E::Id, SyncHistogram<u64>>>) -> R,
    {
        // NOTE; we have to be careful about deadlocks here. if the user's f calls
        // SyncHistogram::refresh(), it is going to wait for every _current_ Recorder to submit at
        // least one more sample, so we need to make sure we're not preventing that! this lock is
        // not taken by any other part of the subscriber, so we _should_ be all good.
        let mut master = self.histograms.lock().unwrap();
        f(&mut master.histograms)
    }
}

impl<NH, S, E> Subscriber for Sampler<NH, S, E>
where
    S: SpanGroup + 'static,
    E: EventGroup + 'static,
    S::Id: Clone + Hash + Eq + 'static,
    E::Id: Clone + Hash + Eq + 'static,
    NH: FnMut() -> Histogram<u64> + Send + 'static,
{
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn new_span(&self, span: &span::Attributes) -> span::Id {
        let mut inner = self.shared.write().unwrap();
        let id = inner
            .last_event
            .insert(atomic::AtomicU64::new(self.time.now()));
        let id2 = inner.refcount.insert(atomic::AtomicUsize::new(1));
        assert_eq!(id, id2);
        let id = span::Id::from_u64(id as u64 + 1);
        let sg = self.span_group.group(span);
        inner.spans.insert(id.clone(), sg.clone());
        inner
            .recorders
            .entry(sg.clone())
            .or_insert_with(HashMap::default);
        inner
            .idle_recorders
            .entry(sg)
            .or_insert_with(HashMap::default);
        id
    }

    fn record(&self, _: &span::Id, _: &span::Record) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event) {
        SPAN.with(|current_span| {
            let current_span = current_span.borrow();
            if let Some(ref span) = *current_span {
                if let Some(time) = self.time_event(span) {
                    self.with_recorder(span, event, |r| {
                        r.saturating_record(time);
                    });
                }
            } else {
                // recorded free-standing event -- ignoring
            }
        })
    }

    fn enter(&self, span: &span::Id) {
        SPAN.with(|current_span| {
            let mut current_span = current_span.borrow_mut();
            if let Some(_cs) = current_span.take() {
                // we entered a span while already in a span
                // let's just keep the inner span
                // TODO: make this configurable or something?
            }
            *current_span = Some(span.clone());
        })
    }

    fn exit(&self, span: &span::Id) {
        SPAN.with(|current_span| {
            let mut current_span = current_span.borrow_mut();
            if let Some(cs) = current_span.take() {
                assert_eq!(&cs, span);
            }
        })
    }

    fn clone_span(&self, span: &span::Id) -> span::Id {
        let inner = self.shared.read().unwrap();
        inner.refcount[span.into_u64() as usize - 1].fetch_add(1, atomic::Ordering::SeqCst);
        span.clone()
    }

    fn drop_span(&self, span: span::Id) {
        if 0 == self.shared.read().unwrap().refcount[span.into_u64() as usize - 1]
            .fetch_sub(1, atomic::Ordering::SeqCst)
        {
            // span has ended!
            // reclaim its id
            let mut inner = self.shared.write().unwrap();
            inner.last_event.remove(span.into_u64() as usize - 1);
            inner.refcount.remove(span.into_u64() as usize - 1);
            inner.spans.remove(&span);
            // we _keep_ the entry in inner.recorders in place, since it may be used by other spans
        }
    }
}

#[derive(Default)]
struct ThreadLocalRecorder(HashMap<ThreadId, UnsafeCell<Recorder<u64>>>);

impl Deref for ThreadLocalRecorder {
    type Target = HashMap<ThreadId, UnsafeCell<Recorder<u64>>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ThreadLocalRecorder {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Hash, Eq, PartialEq)]
#[repr(transparent)]
struct ThreadId {
    tid: usize,
    _notsend: PhantomData<UnsafeCell<()>>,
}

impl Default for ThreadId {
    fn default() -> Self {
        MYTID.with(|mytid| {
            let mut mytid = mytid.borrow_mut();
            if let Some(ref mytid) = *mytid {
                ThreadId {
                    tid: *mytid,
                    _notsend: PhantomData,
                }
            } else {
                let tid = TID.with(|tid| tid.fetch_add(1, atomic::Ordering::AcqRel));
                *mytid = Some(tid);
                ThreadId {
                    tid: tid,
                    _notsend: PhantomData,
                }
            }
        })
    }
}

unsafe impl Send for ThreadLocalRecorder {}
unsafe impl Sync for ThreadLocalRecorder {}

#[cfg(test)]
mod test {
    use super::*;
    use tokio_trace::*;

    #[test]
    fn by_target() {
        let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap())
            .events(group::ByTarget)
            .build();
        let mut _type_of_s = if false { Some(&s) } else { None };
        let d = Dispatch::new(s);
        let d2 = d.clone();
        std::thread::spawn(move || {
            dispatcher::with_default(&d2, || loop {
                trace_span!("foo").in_scope(|| {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    trace!("event");
                })
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        _type_of_s = d.downcast_ref();
        _type_of_s.unwrap().with_histograms(|hs| {
            assert_eq!(hs.len(), 1);
            let hs = &mut hs.get_mut("foo").unwrap();
            assert_eq!(hs.len(), 1);

            let h = &mut hs.get_mut("tracing_metrics::test").unwrap();
            h.refresh();
            // ~= 100ms
            assert!(h.value_at_quantile(0.5) > 50_000_000);
            assert!(h.value_at_quantile(0.5) < 150_000_000);
        })
    }

    #[test]
    fn by_message() {
        let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap()).build();
        let mut _type_of_s = if false { Some(&s) } else { None };
        let d = Dispatch::new(s);
        let d2 = d.clone();
        std::thread::spawn(move || {
            dispatcher::with_default(&d2, || loop {
                trace_span!("foo").in_scope(|| {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    trace!("fast");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    trace!("slow");
                })
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        _type_of_s = d.downcast_ref();
        _type_of_s.unwrap().with_histograms(|hs| {
            assert_eq!(hs.len(), 1);
            let hs = &mut hs.get_mut("foo").unwrap();
            assert_eq!(hs.len(), 2);

            let h = &mut hs.get_mut("fast").unwrap();
            h.refresh();
            // ~= 10ms
            assert!(h.value_at_quantile(0.5) > 5_000_000);
            assert!(h.value_at_quantile(0.5) < 15_000_000);

            let h = &mut hs.get_mut("slow").unwrap();
            h.refresh();
            // ~= 100ms
            assert!(h.value_at_quantile(0.5) > 50_000_000);
            assert!(h.value_at_quantile(0.5) < 150_000_000);
        })
    }

    #[test]
    fn pretty() {
        let s = Builder::from(|| Histogram::new_with_max(1_000_000, 2).unwrap()).build();
        let mut _type_of_s = if false { Some(&s) } else { None };
        let d = Dispatch::new(s);
        let d2 = d.clone();
        std::thread::spawn(move || {
            use rand::prelude::*;
            let mut rng = thread_rng();
            let fast = rand::distributions::Normal::new(100_000.0, 50_000.0);
            let slow = rand::distributions::Normal::new(500_000.0, 20_000.0);
            dispatcher::with_default(&d2, || loop {
                let fast = std::time::Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
                let slow = std::time::Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
                trace_span!("request").in_scope(|| {
                    std::thread::sleep(fast);
                    trace!("fast");
                    std::thread::sleep(slow);
                    trace!("slow");
                })
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        _type_of_s = d.downcast_ref();
        _type_of_s.unwrap().with_histograms(|hs| {
            assert_eq!(hs.len(), 1);
            let hs = &mut hs.get_mut("request").unwrap();
            assert_eq!(hs.len(), 2);

            hs.get_mut("fast").unwrap().refresh();
            hs.get_mut("slow").unwrap().refresh();

            println!("fast:");
            let h = &hs["fast"];
            for v in h
                .iter_linear(50_000)
                .take_while(|v| v.value_iterated_to() < 500_000)
            {
                println!(
                    "{:4}µs | {}",
                    (v.value_iterated_to() + 1) / 1_000,
                    "*".repeat(
                        (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).round()
                            as usize
                    )
                );
            }

            println!("slow:");
            let h = &hs["slow"];
            for v in h
                .iter_linear(50_000)
                .skip_while(|v| v.value_iterated_to() < 500_000)
                .take_while(|v| v.value_iterated_to() < 900_000)
            {
                println!(
                    "{:4}µs | {}",
                    (v.value_iterated_to() + 1) / 1_000,
                    "*".repeat(
                        (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).round()
                            as usize
                    )
                );
            }
        })
    }
}
