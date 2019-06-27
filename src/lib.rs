//! Inter-event timing metrics.
//!
//! This crate provides a `tracing::Subscriber` that keeps statistics on inter-event timing
//! information. More concretely, given code like this:
//!
//! ```rust
//! use tracing::*;
//! use tracing_timing::{Builder, Histogram};
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
//! When [`TimingSubscriber`] is used as the `tracing::Dispatch`, the time between each event in a
//! span is measured using [`quanta`], and is recorded in "[high dynamic range histograms]" using
//! [`hdrhistogram`]'s multi-threaded recording facilities. The recorded timing information is
//! grouped using the [`SpanGroup`] and [`EventGroup`] traits, allowing you to combine recorded
//! statistics across spans and events.
//!
//! ## Extracting timing histograms
//!
//! The crate does not implement a mechanism for recording the resulting histograms. Instead, you
//! can implement this as you see fit using [`TimingSubscriber::with_histograms`]. It gives you
//! access to the histograms for all groups. Note that you must call `refresh()` on each histogram
//! to see its latest values (see [`hdrhistogram::SyncHistogram`]).
//!
//! To access the histograms, you (currently) need to use `tracing::Dispatch::downcast_ref`.
//! The mystical black magic invocations you need to issue are as follows:
//!
//! ```rust
//! use tracing::*;
//! use tracing_timing::{Builder, Histogram};
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
//! By default, [`TimingSubscriber`] groups samples by the "name" of the containing span and the
//! "message" of the relevant event. These are the first string parameter you pass to each of the
//! relevant tracing macros. You can override this behavior either by providing your own
//! implementation of [`SpanGroup`] and [`EventGroup`] to [`Builder::spans`] and
//! [`Builder::events`] respectively. There are also a number of pre-defined "groupers" in the
//! [`group`] module that cover the most common cases.
//!
//! # Timing information over time
//!
//! Every time you refresh a histogram, it incorporates any new timing samples recorded since the
//! last call to `refresh`, allowing you to see timing metrics across all time. If you are
//! monitoring the health of a continuously running system, you may instead wish to only see
//! metrics across a limited window of time. You can do this by clearing the histogram in
//! [`TimingSubscriber::with_histograms`] before refreshing them, or periodically as you see fit.
//!
//!   [high dynamic range histograms]: https://hdrhistogram.github.io/HdrHistogram/
//!   [`hdrhistogram`]: https://docs.rs/hdrhistogram/
//!   [`quanta`]: https://docs.rs/quanta/
//!   [`hdrhistogram::SyncHistogram`]: https://docs.rs/hdrhistogram/6/hdrhistogram/sync/struct.SyncHistogram.html
//!

#![deny(missing_docs)]

use crossbeam::channel;
use crossbeam::sync::ShardedLock;
use hdrhistogram::{sync::Recorder, SyncHistogram};
use slab::Slab;
use std::cell::{RefCell, UnsafeCell};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{atomic, Mutex};
use tracing_core::*;

type HashMap<K, V> = std::collections::HashMap<K, V, fxhash::FxBuildHasher>;

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

/// Translate attributes from a tracing span into a timing span group.
///
/// All spans whose attributes produce the same `Id`-typed value when passed through `group`
/// share a namespace for the groups produced by [`EventGroup::group`] on their contained events.
pub trait SpanGroup {
    /// The type of the timing span group.
    type Id;

    /// Extract the group for this span's attributes.
    fn group(&self, span: &span::Attributes) -> Self::Id;
}

/// Translate attributes from a tracing event into a timing event group.
///
/// All events that share a [`SpanGroup`], and whose attributes produce the same `Id`-typed value
/// when passed through `group`, are considered a single timing target, and have their samples
/// recorded together.
pub trait EventGroup {
    /// The type of the timing event group.
    type Id;

    /// Extract the group for this event.
    fn group(&self, event: &Event) -> Self::Id;
}

fn span_id_to_slab_idx(span: &span::Id) -> usize {
    span.into_u64() as usize - 1
}

struct WriterState<NH, S, E> {
    // We need fast access to the last event for each span.
    last_event: Slab<atomic::AtomicU64>,

    // how many references are there to each span id?
    // needed so we know when to reclaim
    refcount: Slab<atomic::AtomicUsize>,

    // note that many span::Ids can map to the same S
    spans: Slab<S>,

    // TID => (S + callsite) => E => thread-local Recorder
    tls: ThreadLocal<Map<S, E, Recorder<u64>>>,

    // used to produce a Recorder for a thread that has not recorded for a given sid/eid pair
    idle_recorders: Map<S, E, hdrhistogram::sync::IdleRecorder<Recorder<u64>, u64>>,

    // used to communicate new histograms to the reader
    created: channel::Sender<(S, E, SyncHistogram<u64>)>,

    // used to produce a new Histogram when a new sid/eid pair is encountered
    //
    // TODO:
    // placing this in a ShardedLock requires that it is Sync, but it's only ever used when you're
    // holding the write lock. not sure how to describe this in the type system.
    new_histogram: NH,
}

struct ReaderState<S, E> {
    created: channel::Receiver<(S, E, SyncHistogram<u64>)>,
    histograms: HashMap<S, HashMap<E, SyncHistogram<u64>>>,
}

/// Timing-gathering tracing subscriber.
///
/// This type is constructed using a [`Builder`].
///
/// See the [crate-level docs] for details.
///
///   [crate-level docs]: ../
pub struct TimingSubscriber<NH, S = group::ByName, E = group::ByMessage>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    span_group: S,
    event_group: E,
    time: quanta::Clock,

    writers: ShardedLock<WriterState<NH, S::Id, E::Id>>,
    reader: Mutex<ReaderState<S::Id, E::Id>>,
}

impl<NH, S, E> TimingSubscriber<NH, S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
    NH: FnMut() -> Histogram<u64> + Send,
{
    fn time(&self, span: &span::Id, event: &Event) {
        let now = self.time.now();
        let inner = self.writers.read().unwrap();
        let previous =
            inner.last_event[span_id_to_slab_idx(span)].swap(now, atomic::Ordering::AcqRel);
        if previous > now {
            // someone else recorded a sample _just_ now
            // the delta is effectively zero, but recording a 0 sample is misleading
            return;
        }

        let time = now - previous;

        // time to record a sample!
        let f = move |r: &mut Recorder<u64>| r.saturating_record(time);

        // who are we?
        let tid = ThreadId::default();

        // fast path: sid/eid pair is known to this thread
        let eid = self.event_group.group(event);
        if let Some(ref tls) = inner.tls.get(&tid) {
            let sid = &inner.spans[span_id_to_slab_idx(span)];

            // we know no-one else has our TID:
            let tls = unsafe { &mut *tls.get() };

            // the span id _must_ be known, as it's added when created
            if let Some(ref mut recorder) = tls.get_mut(&sid).and_then(|rs| rs.get_mut(&eid)) {
                // sid/eid already known and we already have a thread-local recorder!
                f(recorder);
                return;
            } else if let Some(ref ir) = inner.idle_recorders[&sid].get(&eid) {
                // we didn't know about the eid, but if there's already a recorder for it,
                // we can just create a local recorder from it and move on
                let mut recorder = ir.recorder();
                f(&mut recorder);
                let r = tls
                    .entry(sid.clone())
                    .or_insert_with(Default::default)
                    .insert(eid, recorder);
                assert!(r.is_none());
                return;
            } else {
                // we're the first thread to see this pair, so we need to make a histogram for it
            }
        } else {
            // this thread does not yet have TLS -- we'll have to take the lock
        }

        // slow path: either this thread is new, or the sid/eid pair was new
        // in either case, we need to take the write lock
        // to do that, we must first drop the read lock
        drop(inner);
        let mut inner = self.writers.write().unwrap();
        let inner = &mut *inner;

        // if we don't have any thread-local state, construct that first
        let tls = inner.tls.entry(tid).or_insert_with(Default::default);
        // no-one else has our TID _and_ we have exclusive access to inner
        let tls = unsafe { &mut *tls.get() };

        // use an existing recorder if one exists, or make a new histogram if one does not
        let sid = &inner.spans[span_id_to_slab_idx(span)];
        let nh = &mut inner.new_histogram;
        let created = &mut inner.created;
        let ir = inner
            .idle_recorders
            .get_mut(&inner.spans[span_id_to_slab_idx(span)])
            .unwrap()
            .entry(eid.clone())
            .or_insert_with(|| {
                let h = (nh)().into_sync();
                let ir = h.recorder().into_idle();
                created.send((sid.clone(), eid.clone(), h)).expect(
                    "a WriterState implies there's also a ReaderState, which holds the receiver",
                );
                ir
            });

        // finally, get us a thread-local recorder
        let mut recorder = ir.recorder();
        f(&mut recorder);

        // and stash it away for next time
        let r = tls
            .entry(sid.clone())
            .or_insert_with(Default::default)
            .insert(eid, recorder);
        assert!(r.is_none());
    }

    /// Access the timing histograms.
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
        // writers never take this lock, so we don't hold them up should the user call refresh(),
        let mut reader = self.reader.lock().unwrap();
        while let Ok((sid, eid, h)) = reader.created.try_recv() {
            let h = reader
                .histograms
                .entry(sid)
                .or_insert_with(HashMap::default)
                .insert(eid, h);
            assert!(
                h.is_none(),
                "second histogram created for same sid/eid combination"
            );
        }
        f(&mut reader.histograms)
    }
}

impl<NH, S, E> Subscriber for TimingSubscriber<NH, S, E>
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
        let mut inner = self.writers.write().unwrap();
        let id = inner
            .last_event
            .insert(atomic::AtomicU64::new(self.time.now()));
        let id2 = inner.refcount.insert(atomic::AtomicUsize::new(1));
        assert_eq!(id, id2);
        let sg = self.span_group.group(span);
        let id2 = inner.spans.insert(sg.clone());
        assert_eq!(id, id2);
        let id = span::Id::from_u64(id as u64 + 1);
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
                self.time(span, event);
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
        let inner = self.writers.read().unwrap();
        inner.refcount[span_id_to_slab_idx(span)].fetch_add(1, atomic::Ordering::AcqRel);
        span.clone()
    }

    fn drop_span(&self, span: span::Id) {
        if 0 == self.writers.read().unwrap().refcount[span_id_to_slab_idx(&span)]
            .fetch_sub(1, atomic::Ordering::AcqRel)
        {
            // span has ended!
            // reclaim its id
            let mut inner = self.writers.write().unwrap();
            inner.last_event.remove(span_id_to_slab_idx(&span));
            inner.refcount.remove(span_id_to_slab_idx(&span));
            inner.spans.remove(span_id_to_slab_idx(&span));
            // we _keep_ the entry in inner.recorders in place, since it may be used by other spans
        }
    }
}

#[derive(Hash, Eq, PartialEq, Ord, PartialOrd)]
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

#[derive(Default)]
struct ThreadLocal<T>(HashMap<ThreadId, UnsafeCell<T>>);

impl<T> Deref for ThreadLocal<T> {
    type Target = HashMap<ThreadId, UnsafeCell<T>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for ThreadLocal<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

unsafe impl<T: Send> Send for ThreadLocal<T> {}
unsafe impl<T: Sync> Sync for ThreadLocal<T> {}

#[cfg(test)]
mod test {
    use super::*;
    use tracing::*;

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

            let h = &mut hs.get_mut("tracing_timing::test").unwrap();
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
}
