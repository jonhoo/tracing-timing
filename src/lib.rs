//! Inter-event timing metrics on top of [`tracing`].
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
//! to see its latest values (see [`hdrhistogram::SyncHistogram`]). Note that calling `refresh()`
//! will block until the next event is posted, so you may want [`TimingSubscriber::force_synchronize`]
//! instead.
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
//! // code that hands off clones of the dispatch, maybe to other threads.
//! // for example:
//! tracing::dispatcher::set_global_default(dispatch.clone())
//!   .expect("setting tracing default failed");
//! // (note that Dispatch implements Clone,  so you can keep a handle to it!)
//! //
//! // then, at some later time, in some other place, you can call:
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
//! ## Using the [`Layer`](https://docs.rs/tracing-subscriber/0.2.0/tracing_subscriber/layer/trait.Layer.html) API
//!
//! To use the `Layer` API from `tracing-subscriber`, you first need to enable the `layer` feature.
//! Then, use [`Builder::layer`] to construct a layer that you can mix in with other layers:
//!
//! ```rust
//! # #[cfg(feature = "layer")] {
//! # use tracing::*;
//! # use tracing_timing::{Builder, Histogram};
//! use tracing_subscriber::{registry::Registry, Layer};
//! let timing_layer = Builder::default()
//!     .layer(|| Histogram::new_with_max(1_000_000, 2).unwrap());
//! let dispatch = Dispatch::new(timing_layer.with_subscriber(Registry::default()));
//! # }
//! ```
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
//! ## Interpreting the output
//!
//! To understand the output you get from `tracing-timing`, a more complicated example might help.
//! Consider the following tracing code:
//!
//! ```no_run
//! # let rand = || 0.5;
//! # use tracing::{trace_span, trace};
//! loop {
//!     trace_span!("foo").in_scope(|| {
//!         trace!("foo_start");
//!         std::thread::sleep(std::time::Duration::from_millis(1));
//!         trace_span!("bar").in_scope(|| {
//!             trace!("bar_start");
//!             std::thread::sleep(std::time::Duration::from_millis(1));
//!             if rand() > 0.5 {
//!                 trace!("bar_mid1");
//!                 std::thread::sleep(std::time::Duration::from_millis(1));
//!             } else {
//!                 trace!("bar_mid2");
//!                 std::thread::sleep(std::time::Duration::from_millis(2));
//!             }
//!             trace!("bar_end");
//!         });
//!         trace!("foo_end");
//!     })
//! }
//! ```
//!
//! What histogram data would you expect to see for each event?
//!
//! Well, let's walk through it. There are two span groups: "foo" and "bar". "bar" will contain
//! entries for the events "bar_start", the "bar_mid"s, and "bar_end". "foo" will contain entries
//! for the events "foo_start", "foo_end", **and** all the "bar" events by default (see
//! [`Builder::no_span_recursion`]). Let's look at each of those in turn:
//!
//!  - "foo_start" is easy: it tracks the time since "foo" was created.
//!  - "foo_end" is mostly easy: it tracks the time since "bar_end".
//!    If span recursion is disabled, it instead tracks the time since "foo_start".
//!  - "bar_start" is trickier: in "bar", it tracks the time since "bar" was entered. in "foo",
//!    it contains the time since "foo_start".
//!  - the "bar_mid"s are easy: they both track the time since "bar_start".
//!  - "bar_end" is tricky: it tracks the time since whichever event of "bar_mid1" and "bar_mid2"
//!    happened! The histogram will show a bi-modal distribution with one peak at 1ms, and one peak
//!    at 2ms. If you want to be able to distinguish these, you will have to insert additional
//!    tracing events inside the branches.
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
//!   [`tracing`]: https://docs.rs/tracing/
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
use std::cell::{RefCell, UnsafeCell};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::{atomic, Mutex};
use tracing_core::*;

// also test README.md code example
use doc_comment::doc_comment;
doc_comment!(include_str!("../README.md"));

/// A faster hasher for `tracing-timing` maps.
pub type Hasher = fxhash::FxBuildHasher;

/// A standard library `HashMap` with a faster hasher.
pub type HashMap<K, V> = std::collections::HashMap<K, V, Hasher>;

static TID: atomic::AtomicUsize = atomic::AtomicUsize::new(0);

thread_local! {
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

struct WriterState<S: Hash + Eq, E: Hash + Eq> {
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

impl<S, E> std::fmt::Debug for WriterState<S, E>
where
    S: Hash + Eq + std::fmt::Debug,
    E: Hash + Eq + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterState")
            .field("idle_recorders", &self.idle_recorders)
            .field("created", &self.created)
            .finish()
    }
}

#[derive(Debug)]
struct ReaderState<S: Hash + Eq, E: Hash + Eq> {
    created: channel::Receiver<(S, E, SyncHistogram<u64>)>,
    histograms: HashMap<S, IndexMap<E, SyncHistogram<u64>, Hasher>>,
}

#[derive(Debug)]
struct SpanState<S> {
    group: S,

    // We need fast access to the last event for each span.
    last_event: atomic::AtomicU64,
}

// Share impl between Subscriber and Layer versions
struct Timing<S = group::ByName, E = group::ByMessage>
where
    S: SpanGroup,
    S::Id: Hash + Eq,
    E: EventGroup,
    E::Id: Hash + Eq,
{
    span_group: S,
    event_group: E,
    time: quanta::Clock,
    bubble_spans: bool,
    span_close_events: bool,

    writers: ShardedLock<WriterState<S::Id, E::Id>>,
    reader: Mutex<ReaderState<S::Id, E::Id>>,
}

impl<S, E> std::fmt::Debug for Timing<S, E>
where
    S: SpanGroup + std::fmt::Debug,
    E: EventGroup + std::fmt::Debug,
    S::Id: Hash + Eq + std::fmt::Debug,
    E::Id: Hash + Eq + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timing")
            .field("span_group", &self.span_group)
            .field("event_group", &self.event_group)
            .field("time", &self.time)
            .field("writers", &self.writers)
            .field("reader", &self.reader)
            .finish()
    }
}

impl<S, E> Timing<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    fn force_synchronize(&self) {
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

    fn with_histograms<F, R>(&self, f: F) -> R
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

impl<S, E> Timing<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
{
    fn time<'a, F>(&self, event: &Event, mut for_each_parent: F)
    where
        S: 'a,
        F: FnMut(&mut dyn FnMut(&SpanState<S::Id>) -> bool),
    {
        let start = self.time.start();
        let inner = self.writers.read().unwrap();

        let record = move |state: &SpanState<S::Id>, r: &mut Recorder<u64>| {
            // NOTE: we substitute in the last possible timestamp to avoid measuring time spent
            // in accessing the various timing datastructures (like taking locks). this has the
            // effect of measuing Δt₁₂ = e₂.start - e₁.end, which is probably what users expect
            let previous = state
                .last_event
                .swap(self.time.end(), atomic::Ordering::AcqRel);
            if previous > start {
                // someone else recorded a sample _just_ now
                // the delta is effectively zero, but recording a 0 sample is misleading
                return;
            }

            r.saturating_record(self.time.delta(previous, start).as_nanos() as u64)
        };

        // who are we?
        let tid = ThreadId::default();

        // fast path: sid/eid pair is known to this thread
        let eid = self.event_group.group(event);
        if let Some(ref tls) = inner.tls.get(&tid) {
            // we know no-one else has our TID:
            // NOTE: it's _not_ safe to use this after we drop the lock due to force_synchronize.
            let tls = unsafe { &mut *tls.get() };

            for_each_parent(&mut |state| {
                // the span id _must_ be known, as it's added when created
                if let Some(ref mut recorder) =
                    tls.get_mut(&state.group).and_then(|rs| rs.get_mut(&eid))
                {
                    // sid/eid already known and we already have a thread-local recorder!
                    record(state, recorder);
                } else if let Some(ref ir) = inner.idle_recorders[&state.group].get(&eid) {
                    // we didn't know about the eid, but if there's already a recorder for it,
                    // we can just create a local recorder from it and move on
                    let mut recorder = ir.recorder();
                    record(state, &mut recorder);
                    let r = tls
                        .entry(state.group.clone())
                        .or_insert_with(Default::default)
                        .insert(eid.clone(), recorder);
                    assert!(r.is_none());
                } else {
                    // we're the first thread to see this pair, so we need to make a histogram for it
                    return false;
                }

                // keep recording up the stack
                self.bubble_spans
            });

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
        for_each_parent(&mut |state| {
            // since we're recursing up the tree, we _may_ find that we already have a recorder for
            // a _later_ span's sid/eid. make sure we don't create a new one in that case!
            let recorder = tls
                .entry(state.group.clone())
                .or_insert_with(Default::default)
                .entry(eid.clone())
                .or_insert_with(|| {
                    // nope, get us a thread-local recorder
                    idle.get_mut(&state.group)
                        .unwrap()
                        .entry(eid.clone())
                        .or_insert_with(|| {
                            // no histogram exists! make one.
                            let h = (nh)(&state.group, &eid).into_sync();
                            let ir = h.recorder().into_idle();
                            created.send((state.group.clone(), eid.clone(), h)).expect(
                                "WriterState implies ReaderState, which holds the receiver",
                            );
                            ir
                        })
                        .recorder()
                });

            // finally, we can record the sample
            record(state, recorder);

            // recurse to parent if any
            self.bubble_spans
        });
    }

    fn ensure_group(&self, group: S::Id) {
        self.writers
            .write()
            .unwrap()
            .idle_recorders
            .entry(group)
            .or_insert_with(HashMap::default);
    }
}

mod subscriber;
pub use subscriber::{Downcaster as SubscriberDowncaster, TimingSubscriber};

#[cfg(feature = "layer")]
mod layer;
#[cfg(feature = "layer")]
pub use layer::{Downcaster as LayerDowncaster, TimingLayer};

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
