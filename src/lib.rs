//! Inter-event timing metrics.
//!
//! This crate provides a `tracing::Subscriber` that keeps statistics on inter-event timing
//! information. More concretely, given code like this:
//!
//! ```rust
//! use tracing::*;
//! use tracing_timing::{Builder, Histogram};
//! let subscriber = Builder::default().build(|| Histogram::new_with_max(1_000_000, 2).unwrap());
//! let dispatcher = Dispatch::new(subscriber);
//! dispatcher::with_default(&dispatcher, || {
//!     trace_span!("request").in_scope(|| {
//!         // do a little bit of work
//!         trace!("fast");
//!         // do a lot of work
//!         trace!("slow");
//!     })
//! });
//! ```
//!
//! You can produce something like this (see `examples/pretty.rs`):
//!
//! ```text
//! fast:
//! mean: 173.2µs, p50: 172µs, p90: 262µs, p99: 327µs, p999: 450µs, max: 778µs
//!   25µs | *                                        |  2.2th %-ile
//!   50µs | *                                        |  2.2th %-ile
//!   75µs | *                                        |  4.7th %-ile
//!  100µs | ***                                      | 11.5th %-ile
//!  125µs | *****                                    | 24.0th %-ile
//!  150µs | *******                                  | 41.1th %-ile
//!  175µs | ********                                 | 59.2th %-ile
//!  200µs | *******                                  | 75.4th %-ile
//!  225µs | **                                       | 80.1th %-ile
//!  250µs | ***                                      | 87.3th %-ile
//!  275µs | ***                                      | 94.4th %-ile
//!  300µs | **                                       | 97.8th %-ile
//!
//! slow:
//! mean: 623.3µs, p50: 630µs, p90: 696µs, p99: 770µs, p999: 851µs, max: 950µs
//!  500µs | *                                        |  1.6th %-ile
//!  525µs | **                                       |  4.8th %-ile
//!  550µs | ***                                      | 10.9th %-ile
//!  575µs | *****                                    | 22.2th %-ile
//!  600µs | *******                                  | 37.9th %-ile
//!  625µs | ********                                 | 55.9th %-ile
//!  650µs | *******                                  | 72.9th %-ile
//!  675µs | ******                                   | 85.6th %-ile
//!  700µs | ****                                     | 93.5th %-ile
//!  725µs | **                                       | 97.1th %-ile
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
//! To access the histograms later, use `tracing::Dispatch::downcast_ref`. If your type is hard to
//! name, you can use a [`TimingSubscriber::downcaster`] instead.
//!
//! ```rust
//! use tracing::*;
//! use tracing_timing::{Builder, Histogram, TimingSubscriber};
//! let subscriber = Builder::default().build(|| Histogram::new_with_max(1_000_000, 2).unwrap());
//! let dispatch = Dispatch::new(subscriber);
//! // ...
//! // code that hands off clones of the dispatch
//! // maybe to other threads
//! // ...
//! dispatch.downcast_ref::<TimingSubscriber>().unwrap().with_histograms(|hs| {
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
//! # Usage notes:
//!
//! **Event timing is _per span_, not per span _per thread_.** This means that if you emit events
//! for the same span concurrently from multiple threads, you may see weird timing information.
//!
//! **Span creation takes a lock.** This means that you will generally want to avoid creating
//! extraneous spans. One technique that works well here is subsampling your application, for
//! example by only creating tracking spans for _some_ of your requests.
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
use indexmap::IndexMap;
use slab::Slab;
use std::cell::{RefCell, UnsafeCell};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{atomic, Mutex};
use tracing_core::*;

/// A faster hasher for `tracing-timing` maps.
pub type Hasher = fxhash::FxBuildHasher;

/// A standard library `HashMap` with a faster hasher.
pub type HashMap<K, V> = std::collections::HashMap<K, V, Hasher>;

static TID: atomic::AtomicUsize = atomic::AtomicUsize::new(0);

thread_local! {
    static SPAN: RefCell<Vec<span::Id>> = RefCell::new(Vec::new());
    static MYTID: RefCell<Option<usize>> = RefCell::new(None);
}

mod builder;
pub use builder::Builder;
pub use hdrhistogram::Histogram;

pub mod group;

#[derive(Debug, Clone)]
struct SpanGroupIdent<S> {
    group: S,
    parent: Option<span::Id>,
    follows: Option<span::Id>,
}

type Map<S, E, T> = HashMap<S, HashMap<E, T>>;

/// Translate attributes from a tracing span into a timing span group.
///
/// All spans whose attributes produce the same `Id`-typed value when passed through `group`
/// share a namespace for the groups produced by [`EventGroup::group`] on their contained events.
///
/// This trait is implemented for all functions with the appropriate signature. Note that you _may_
/// run into weird lifetime errors from the compiler when using a closure as a `SpanGroup`. This is
/// a [known compiler issue]. You can work around it by adding a slight type hint to the arguments
/// passed to the closure as follows (note the `: &_`):
///
/// ```rust
/// use tracing_timing::{Builder, Histogram};
/// use tracing::span;
/// let s = Builder::default()
///     .spans(|_: &span::Attributes| "all spans as one")
///     .build(|| Histogram::new(3).unwrap());
/// ```
///
///   [known compiler issue]: https://github.com/rust-lang/rust/issues/41078
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
///
/// This trait is implemented for all functions with the appropriate signature. Note that you _may_
/// run into weird lifetime errors from the compiler when using a closure as an `EventGroup`. This
/// is a [known compiler issue]. You can work around it by adding a slight type hint to the
/// arguments passed to the closure as follows (note the `: &_`):
///
/// ```rust
/// use tracing_timing::{Builder, Histogram};
/// use tracing::Event;
/// let s = Builder::default()
///     .events(|_: &Event| "all events as one")
///     .build(|| Histogram::new(3).unwrap());
/// ```
///
///   [known compiler issue]: https://github.com/rust-lang/rust/issues/41078
pub trait EventGroup {
    /// The type of the timing event group.
    type Id;

    /// Extract the group for this event.
    fn group(&self, event: &Event) -> Self::Id;
}

fn span_id_to_slab_idx(span: &span::Id) -> usize {
    span.into_u64() as usize - 1
}

struct WriterState<S, E> {
    // We need fast access to the last event for each span.
    last_event: Slab<atomic::AtomicU64>,

    // how many references are there to each span id?
    // needed so we know when to reclaim
    refcount: Slab<atomic::AtomicUsize>,

    // note that many span::Ids can map to the same S
    spans: Slab<SpanGroupIdent<S>>,

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
    new_histogram: Box<dyn FnMut(&S, &E) -> Histogram<u64> + Send + Sync>,
}

struct ReaderState<S, E> {
    created: channel::Receiver<(S, E, SyncHistogram<u64>)>,
    histograms: HashMap<S, IndexMap<E, SyncHistogram<u64>, Hasher>>,
}

/// Timing-gathering tracing subscriber.
///
/// This type is constructed using a [`Builder`].
///
/// See the [crate-level docs] for details.
///
///   [crate-level docs]: ../
pub struct TimingSubscriber<S = group::ByName, E = group::ByMessage>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    span_group: S,
    event_group: E,
    time: quanta::Clock,

    writers: ShardedLock<WriterState<S::Id, E::Id>>,
    reader: Mutex<ReaderState<S::Id, E::Id>>,
}

impl<S, E> TimingSubscriber<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
{
    fn time(&self, mut span: span::Id, event: &Event) {
        let now = self.time.now();
        let inner = self.writers.read().unwrap();

        let record =
            move |last_event: &Slab<atomic::AtomicU64>, r: &mut Recorder<u64>, span: &span::Id| {
                let previous =
                    last_event[span_id_to_slab_idx(span)].swap(now, atomic::Ordering::AcqRel);
                if previous > now {
                    // someone else recorded a sample _just_ now
                    // the delta is effectively zero, but recording a 0 sample is misleading
                    return;
                }
                r.saturating_record(now - previous)
            };

        // who are we?
        let tid = ThreadId::default();

        // fast path: sid/eid pair is known to this thread
        let eid = self.event_group.group(event);
        if let Some(ref tls) = inner.tls.get(&tid) {
            // we know no-one else has our TID:
            // NOTE: it's _not_ safe to use this after we drop the lock due to force_synchronize.
            let tls = unsafe { &mut *tls.get() };

            loop {
                // the span id _must_ be known, as it's added when created
                let sgi = &inner.spans[span_id_to_slab_idx(&span)];
                if let Some(ref mut recorder) =
                    tls.get_mut(&sgi.group).and_then(|rs| rs.get_mut(&eid))
                {
                    // sid/eid already known and we already have a thread-local recorder!
                    record(&inner.last_event, recorder, &span);
                } else if let Some(ref ir) = inner.idle_recorders[&sgi.group].get(&eid) {
                    // we didn't know about the eid, but if there's already a recorder for it,
                    // we can just create a local recorder from it and move on
                    let mut recorder = ir.recorder();
                    record(&inner.last_event, &mut recorder, &span);
                    let r = tls
                        .entry(sgi.group.clone())
                        .or_insert_with(Default::default)
                        .insert(eid.clone(), recorder);
                    assert!(r.is_none());
                } else {
                    // we're the first thread to see this pair, so we need to make a histogram for it
                    break;
                }

                if let Some(ref psi) = sgi.parent {
                    // keep recording up the stack
                    span = psi.clone();
                } else {
                    return;
                }
            }

        // at least one sid/eid pair was unknown
        } else {
            // this thread does not yet have TLS -- we'll have to take the lock
        }

        // slow path: either this thread is new, or a sid/eid pair was new
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
        let nh = &mut inner.new_histogram;
        let created = &mut inner.created;
        let idle = &mut inner.idle_recorders;
        loop {
            let sgi = &inner.spans[span_id_to_slab_idx(&span)];

            // since we're recursing up the tree, we _may_ find that we already have a recorder for
            // a _later_ span's sid/eid. make sure we don't create a new one in that case!
            let recorder = tls
                .entry(sgi.group.clone())
                .or_insert_with(Default::default)
                .entry(eid.clone())
                .or_insert_with(|| {
                    // nope, get us a thread-local recorder
                    idle.get_mut(&sgi.group)
                        .unwrap()
                        .entry(eid.clone())
                        .or_insert_with(|| {
                            // no histogram exists! make one.
                            let h = (nh)(&sgi.group, &eid).into_sync();
                            let ir = h.recorder().into_idle();
                            created.send((sgi.group.clone(), eid.clone(), h)).expect(
                                "WriterState implies ReaderState, which holds the receiver",
                            );
                            ir
                        })
                        .recorder()
                });

            // finally, we can record the sample
            record(&inner.last_event, recorder, &span);

            // recurse to parent if any
            if let Some(ref psi) = sgi.parent {
                span = psi.clone();
            } else {
                break;
            }
        }
    }

    /// Force all current timing information to be refreshed immediately.
    ///
    /// Note that this will interrupt all concurrent metrics gathering until it returns.
    pub fn force_synchronize(&self) {
        // first, remove all thread-local recorders
        let mut inner = self.writers.write().unwrap();
        // note that we don't remove the tls _entry_,
        // since that would make all writers take the write lock later!
        for tls in inner.tls.values_mut() {
            // we hold the write lock, so we know no other thread is using its tls.
            let tls = unsafe { &mut *tls.get() };
            tls.clear();
        }
        // now that we've done that, refresh all the histograms. we do it with a 0 timeout since we
        // know that dropping all the recorders above will cause refresh to see up-to-date values,
        // and we don't care about samples coming _after_ the clear above.
        drop(inner);
        self.with_histograms(|hs| {
            for hs in hs.values_mut() {
                for h in hs.values_mut() {
                    h.refresh_timeout(std::time::Duration::new(0, 0));
                }
            }
        })
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
        F: FnOnce(&mut HashMap<S::Id, IndexMap<E::Id, SyncHistogram<u64>, Hasher>>) -> R,
    {
        // writers never take this lock, so we don't hold them up should the user call refresh(),
        let mut reader = self.reader.lock().unwrap();
        while let Ok((sid, eid, h)) = reader.created.try_recv() {
            let h = reader
                .histograms
                .entry(sid)
                .or_insert_with(IndexMap::default)
                .insert(eid, h);
            assert!(
                h.is_none(),
                "second histogram created for same sid/eid combination"
            );
        }
        f(&mut reader.histograms)
    }
}

impl<S, E> Subscriber for TimingSubscriber<S, E>
where
    S: SpanGroup + 'static,
    E: EventGroup + 'static,
    S::Id: Clone + Hash + Eq + 'static,
    E::Id: Clone + Hash + Eq + 'static,
{
    fn enabled(&self, _: &Metadata) -> bool {
        // TODO: implement support for per-group subsampling
        true
    }

    fn new_span(&self, span: &span::Attributes) -> span::Id {
        let group = self.span_group.group(span);
        let parent = span
            .parent()
            .cloned()
            .or_else(|| SPAN.with(|current_span| current_span.borrow().last().cloned()));
        let sg = SpanGroupIdent {
            group,
            parent,
            follows: None,
        };

        let mut inner = self.writers.write().unwrap();
        let id = inner.refcount.insert(atomic::AtomicUsize::new(1));
        let id2 = inner.spans.insert(sg.clone());
        assert_eq!(id, id2);
        inner
            .idle_recorders
            .entry(sg.group)
            .or_insert_with(HashMap::default);
        let id2 = inner
            .last_event
            .insert(atomic::AtomicU64::new(self.time.now()));
        assert_eq!(id, id2);
        span::Id::from_u64(id as u64 + 1)
    }

    fn record(&self, _: &span::Id, _: &span::Record) {}

    fn record_follows_from(&self, span: &span::Id, follows: &span::Id) {
        let mut inner = self.writers.write().unwrap();
        inner
            .spans
            .get_mut(span_id_to_slab_idx(span))
            .unwrap()
            .follows = Some(follows.clone());
    }

    fn event(&self, event: &Event) {
        let span = event.parent().cloned().or_else(|| {
            SPAN.with(|current_span| {
                let current_span = current_span.borrow();
                current_span.last().cloned()
            })
        });
        if let Some(span) = span {
            self.time(span, event);
        } else {
            // recorded free-standing event -- ignoring
        }
    }

    fn enter(&self, span: &span::Id) {
        SPAN.with(|current_span| {
            current_span.borrow_mut().push(span.clone());
        })
    }

    fn exit(&self, span: &span::Id) {
        // we are guaranteed that one any given thread, spans are exited in reverse order
        SPAN.with(|current_span| {
            let leaving = current_span
                .borrow_mut()
                .pop()
                .expect("told to exit span when not in span");
            assert_eq!(
                &leaving, span,
                "told to exit span that was not most recently entered"
            );
        })
    }

    fn clone_span(&self, span: &span::Id) -> span::Id {
        let inner = self.writers.read().unwrap();
        inner.refcount[span_id_to_slab_idx(span)].fetch_add(1, atomic::Ordering::AcqRel);
        span.clone()
    }

    fn drop_span(&self, span: span::Id) {
        if 1 == self.writers.read().unwrap().refcount[span_id_to_slab_idx(&span)]
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

/// A convenience type for getting access to [`TimingSubscriber`] through a `Dispatch`.
///
/// See [`TimingSubscriber::downcaster`].
#[derive(Debug, Copy)]
pub struct Downcaster<S, E> {
    phantom: PhantomData<fn(S, E)>,
}

impl<S, E> Clone for Downcaster<S, E> {
    fn clone(&self) -> Self {
        Self {
            phantom: PhantomData,
        }
    }
}

impl<S, E> TimingSubscriber<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
{
    /// Returns an identifier that can later be used to get access to this [`TimingSubscriber`]
    /// after it has been turned into a `tracing::Dispatch`.
    ///
    /// ```rust
    /// use tracing::*;
    /// use tracing_timing::{Builder, Histogram, TimingSubscriber};
    /// let subscriber = Builder::default().build(|| Histogram::new_with_max(1_000_000, 2).unwrap());
    /// let downcaster = subscriber.downcaster();
    /// let dispatch = Dispatch::new(subscriber);
    /// // ...
    /// // code that hands off clones of the dispatch
    /// // maybe to other threads
    /// // ...
    /// downcaster.downcast(&dispatch).unwrap().with_histograms(|hs| {
    ///     for (span_group, hs) in hs {
    ///         for (event_group, h) in hs {
    ///             // make sure we see the latest samples:
    ///             h.refresh();
    ///             // print the median:
    ///             println!("{} -> {}: {}ns", span_group, event_group, h.value_at_quantile(0.5))
    ///         }
    ///     }
    /// });
    /// ```
    ///
    pub fn downcaster(&self) -> Downcaster<S, E> {
        Downcaster {
            phantom: PhantomData,
        }
    }
}

impl<S, E> Downcaster<S, E>
where
    S: SpanGroup + 'static,
    E: EventGroup + 'static,
    S::Id: Clone + Hash + Eq + 'static,
    E::Id: Clone + Hash + Eq + 'static,
{
    /// Retrieve a reference to this ident's original [`TimingSubscriber`].
    ///
    /// This method returns `None` if the given `Dispatch` is not holding a subscriber of the same
    /// type as this ident was created from.
    pub fn downcast<'a>(&self, d: &'a Dispatch) -> Option<&'a TimingSubscriber<S, E>> {
        d.downcast_ref()
    }
}

#[derive(Hash, Eq, PartialEq, Ord, PartialOrd, Debug, Copy, Clone)]
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
                let tid = TID.fetch_add(1, atomic::Ordering::AcqRel);
                *mytid = Some(tid);
                ThreadId {
                    tid,
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
