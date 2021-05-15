use super::*;
use crossbeam::sync::ShardedLock;
use hdrhistogram::SyncHistogram;
use indexmap::IndexMap;
use slab::Slab;
use std::cell::RefCell;
use std::hash::Hash;
use std::sync::atomic;

thread_local! {
    static SPAN: RefCell<Vec<span::Id>> = RefCell::new(Vec::new());
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
    spans: ShardedLock<Slab<SpanGroupContext<S::Id>>>,
    timing: Timing<S, E>,
}

impl<S, E> std::fmt::Debug for TimingSubscriber<S, E>
where
    S: SpanGroup + std::fmt::Debug,
    S::Id: Hash + Eq + std::fmt::Debug,
    E: EventGroup + std::fmt::Debug,
    E::Id: Hash + Eq + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimingSubscriber")
            .field("spans", &self.spans)
            .field("timing", &self.timing)
            .finish()
    }
}

#[derive(Debug)]
struct SpanGroupContext<S> {
    parent: Option<span::Id>,
    follows: Option<span::Id>,
    meta: &'static Metadata<'static>,
    state: SpanState<S>,

    // how many references are there to each span id?
    // needed so we know when to reclaim
    refcount: atomic::AtomicUsize,
}

impl<S, E> TimingSubscriber<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    pub(crate) fn new(timing: Timing<S, E>) -> Self {
        Self {
            timing,
            spans: Default::default(),
        }
    }

    /// Force all current timing information to be refreshed immediately.
    ///
    /// Note that this will interrupt all concurrent metrics gathering until it returns.
    pub fn force_synchronize(&self) {
        self.timing.force_synchronize()
    }

    /// Access the timing histograms.
    ///
    /// Be aware that the contained histograms are not automatically updated to reflect recently
    /// gathered samples. For each histogram you wish to read from, you must call `refresh` or
    /// `refresh_timeout` or `force_synchronize` to gather up-to-date samples.
    ///
    /// For information about what you can do with the histograms, see the [`hdrhistogram`
    /// documentation].
    ///
    ///   [`hdrhistogram` documentation]: https://docs.rs/hdrhistogram/
    pub fn with_histograms<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut HashMap<S::Id, IndexMap<E::Id, SyncHistogram<u64>, Hasher>>) -> R,
    {
        self.timing.with_histograms(f)
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
        // NOTE: we override this just because we have to. for filtering, use Layer
        true
    }

    fn new_span(&self, span: &span::Attributes) -> span::Id {
        let group = self.timing.span_group.group(span);
        let parent = span
            .parent()
            .cloned()
            .or_else(|| SPAN.with(|current_span| current_span.borrow().last().cloned()));

        let sg = SpanGroupContext {
            parent,
            follows: None,
            meta: span.metadata(),
            refcount: atomic::AtomicUsize::new(1),
            state: SpanState {
                group: group.clone(),
                last_event: atomic::AtomicU64::new(self.timing.time.raw()),
            },
        };

        let id = {
            let mut inner = self.spans.write().unwrap();
            inner.insert(sg)
        };

        self.timing.ensure_group(group);
        span::Id::from_u64(id as u64 + 1)
    }

    fn record(&self, _: &span::Id, _: &span::Record) {}

    fn record_follows_from(&self, span: &span::Id, follows: &span::Id) {
        let mut inner = self.spans.write().unwrap();
        inner.get_mut(span_id_to_slab_idx(span)).unwrap().follows = Some(follows.clone());
    }

    fn event(&self, event: &Event) {
        let span = event.parent().cloned().or_else(|| {
            SPAN.with(|current_span| {
                let current_span = current_span.borrow();
                current_span.last().cloned()
            })
        });
        if let Some(span) = span {
            let inner = self.spans.read().unwrap();
            let inner = &*inner;
            self.timing.time(event, |on_each| {
                let mut current = Some(span.clone());
                while let Some(ref at) = current {
                    let idx = span_id_to_slab_idx(&at);
                    let span = &inner[idx];
                    if !on_each(&span.state) {
                        break;
                    }
                    current = span.parent.clone();
                }
            });
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
        // we are guaranteed that on any given thread, spans are exited in reverse order
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
        let inner = self.spans.read().unwrap();
        inner[span_id_to_slab_idx(span)]
            .refcount
            .fetch_add(1, atomic::Ordering::AcqRel);
        span.clone()
    }

    fn try_close(&self, span: span::Id) -> bool {
        macro_rules! unwinding_lock {
            ($lock:expr) => {
                match $lock {
                    Ok(g) => g,
                    Err(_) if std::thread::panicking() => {
                        // we're trying to take the span lock while panicking
                        // the lock is poisoned, so the writer state is corrupt
                        // so we might as well just return -- nothing more we can do
                        return false;
                    }
                    r @ Err(_) => r.unwrap(),
                }
            };
        }

        if 1 == unwinding_lock!(self.spans.read())[span_id_to_slab_idx(&span)]
            .refcount
            .fetch_sub(1, atomic::Ordering::AcqRel)
        {
            // span has ended!
            if self.timing.span_close_events {
                // record a span-end event
                let inner = unwinding_lock!(self.spans.read());
                if let Some(span_info) = inner.get(span_id_to_slab_idx(&span)) {
                    let meta = span_info.meta;
                    let fs = field::FieldSet::new(&["message"], meta.callsite());
                    let fld = fs.iter().next().unwrap();
                    let v = [(&fld, Some(&"close" as &dyn field::Value))];
                    let vs = fs.value_set(&v);
                    let e = Event::new_child_of(span.clone(), meta, &vs);
                    self.event(&e);
                }
            }

            // reclaim the span's id
            let mut inner = unwinding_lock!(self.spans.write());
            inner.remove(span_id_to_slab_idx(&span));
            // we _keep_ the entry in inner.recorders in place, since it may be used by other spans
            true
        } else {
            false
        }
    }

    fn current_span(&self) -> span::Current {
        SPAN.with(|current_span| {
            current_span.borrow_mut().last().map(|sid| {
                span::Current::new(
                    sid.clone(),
                    self.spans.read().unwrap()[span_id_to_slab_idx(sid)].meta,
                )
            })
        })
        .unwrap_or_else(span::Current::none)
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
