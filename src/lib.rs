use crossbeam::sync::ShardedLock;
use fnv::FnvHashMap as HashMap;
use hdrhistogram::{
    sync::Recorder,
    {Histogram, SyncHistogram},
};
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

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct SpanGroupIdent<G> {
    callsite: self::callsite::Identifier,
    group: G,
}

mod builder;
pub use builder::Builder;
pub mod group;

pub trait SpanGroup {
    type Id;
    fn group(&self, a: &span::Attributes) -> Self::Id;
}

pub trait EventGroup {
    type Id;
    fn group(&self, e: &Event) -> Self::Id;
}

struct SamplerInner<S, E> {
    /// We need fast access to the last event for each span.
    last_event: Slab<atomic::AtomicU64>,

    // how many references are there to each span id?
    // needed so we know when to reclaim
    refcount: Slab<atomic::AtomicUsize>,

    // note that many span::Ids can map to the same SpanGroupIdent
    spans: HashMap<span::Id, SpanGroupIdent<S>>,

    // (S + callsite) => E => TID => Recorder
    recorders: HashMap<SpanGroupIdent<S>, HashMap<E, ThreadLocalRecorder>>,
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
        }
    }
}

/// For each event, we record the time between it and the preceeding event in the same span.
/// We record that time difference in a histogram keyed by the callsite of the event's span **and**
/// the span's _group_ as dictated by `Group`.
pub struct Sampler<S = group::ByName, E = group::ByTarget>
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
    recorder: hdrhistogram::sync::IdleRecorder<Recorder<u64>, u64>,
    histogram: Mutex<SyncHistogram<u64>>,
}

impl<S, E> Sampler<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    fn time_event(&self, span: &span::Id) -> Option<u64> {
        let now = self.time.now();
        let previous = self.shared.read().unwrap().last_event[span.into_u64() as usize]
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
        // first, get the recorder i
        let eid = self.event_group.group(event);
        let inner = self.shared.read().unwrap();
        // TODO: make event if not exist
        let tmp = &inner.recorders[&inner.spans[span]];
        if let Some(ref recorder) = tmp.get(&eid).and_then(|recorders| recorders.get(&tid)) {
            // we know no-one else has our TID
            f(unsafe { &mut *recorder.get() });
            return;
        }

        // need to create entry for recorder where there was none
        // to do that, we need to take the write lock, so we must drop the read lock
        drop(tmp);
        drop(inner);

        // we're going to need a new recorder
        let mut recorder = self.recorder.recorder();
        f(&mut recorder);

        // store the recorder back for next time
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

    pub fn with_histogram<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Histogram<u64>) -> R,
    {
        // NOTE; we have to be careful not to deadlock here
        // refresh() is going to wait for every _current_ Recorder to submit at least one more
        // sample, so we need to make sure we're not preventing that! this lock is not taken by any
        // other part of the subscriber, so we _should_ be all good.
        let mut histogram = self.histogram.lock().unwrap();
        histogram.refresh();
        f(&mut *histogram)
    }
}

impl<S, E> Subscriber for Sampler<S, E>
where
    S: SpanGroup + 'static,
    E: EventGroup + 'static,
    S::Id: Clone + Hash + Eq + 'static,
    E::Id: Hash + Eq + 'static,
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
        let id = span::Id::from_u64(id as u64);
        let sgi = SpanGroupIdent {
            callsite: span.metadata().callsite(),
            group: self.span_group.group(span),
        };
        inner.spans.insert(id.clone(), sgi.clone());
        inner.recorders.entry(sgi).or_insert_with(HashMap::default);
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
        inner.refcount[span.into_u64() as usize].fetch_add(1, atomic::Ordering::SeqCst);
        span.clone()
    }

    fn drop_span(&self, span: span::Id) {
        if 0 == self.shared.read().unwrap().refcount[span.into_u64() as usize]
            .fetch_sub(1, atomic::Ordering::SeqCst)
        {
            // span has ended!
            // reclaim its id
            let mut inner = self.shared.write().unwrap();
            inner.last_event.remove(span.into_u64() as usize);
            inner.refcount.remove(span.into_u64() as usize);
            inner.spans.remove(&span);
            // NOTE: we _keep_ the SpanGroupIdent in place, since it is probably used by other spans
        }
    }
}

impl<S, E> Drop for Sampler<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    fn drop(&mut self) {}
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
