use super::*;
use hdrhistogram::SyncHistogram;
use indexmap::IndexMap;
use std::hash::Hash;
use std::sync::atomic;

/// Timing-gathering tracing layer.
///
/// This type is constructed using a [`Builder`].
///
/// See the [crate-level docs] for details.
///
pub struct TimingLayer<S = group::ByName, E = group::ByMessage>
where
    S: SpanGroup,
    S::Id: Hash + Eq,
    E: EventGroup,
    E::Id: Hash + Eq,
{
    timing: Timing<S, E>,
}

impl<S, E> std::fmt::Debug for TimingLayer<S, E>
where
    S: SpanGroup + std::fmt::Debug,
    S::Id: Hash + Eq + std::fmt::Debug,
    E: EventGroup + std::fmt::Debug,
    E::Id: Hash + Eq + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimingLayer")
            .field("timing", &self.timing)
            .finish()
    }
}

impl<S, E> TimingLayer<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Hash + Eq,
    E::Id: Hash + Eq,
{
    pub(crate) fn new(timing: Timing<S, E>) -> Self {
        Self { timing }
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
        self.timing.with_histograms(f)
    }
}

impl<S, SG, EG> tracing_subscriber::Layer<S> for TimingLayer<SG, EG>
where
    S: Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    Self: 'static,
    SG: SpanGroup,
    EG: EventGroup,
    SG::Id: Clone + Hash + Eq + Send + Sync,
    EG::Id: Clone + Hash + Eq + Send + Sync,
{
    fn on_new_span(
        &self,
        attrs: &span::Attributes,
        id: &span::Id,
        ctx: tracing_subscriber::layer::Context<S>,
    ) {
        let group = self.timing.span_group.group(attrs);
        self.timing.ensure_group(group.clone());
        let span = ctx.span(id).unwrap();
        span.extensions_mut().insert(SpanState {
            group,
            last_event: atomic::AtomicU64::new(self.timing.time.raw()),
        });
    }

    fn on_event(&self, event: &Event, ctx: tracing_subscriber::layer::Context<S>) {
        let span = event
            .parent()
            .cloned()
            .or_else(|| ctx.current_span().id().cloned());
        if let Some(id) = span {
            let current = ctx.span(&id);
            self.timing.time(event, |on_each| {
                if let Some(ref span) = current {
                    {
                        let ext = span.extensions();
                        if !on_each(ext.get::<SpanState<SG::Id>>().unwrap()) {
                            return;
                        }
                    }

                    for span in span.scope().skip(1) {
                        let ext = span.extensions();
                        if !on_each(ext.get::<SpanState<SG::Id>>().unwrap()) {
                            break;
                        }
                    }
                }
            });
        } else {
            // recorded free-standing event -- ignoring
        }
    }

    fn on_close(&self, id: span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
        if self.timing.span_close_events {
            let span = ctx.span(&id).expect("Span not found, this is a bug");
            let meta = span.metadata();
            let fs = field::FieldSet::new(&["message"], meta.callsite());
            let fld = fs.iter().next().unwrap();
            let v = [(&fld, Some(&"close" as &dyn field::Value))];
            let vs = fs.value_set(&v);
            let e = Event::new_child_of(id, meta, &vs);
            self.timing.time(&e, |on_each| {
                {
                    let ext = span.extensions();
                    if !on_each(ext.get::<SpanState<SG::Id>>().unwrap()) {
                        return;
                    }
                }

                for span in span.scope().skip(1) {
                    let ext = span.extensions();
                    if !on_each(ext.get::<SpanState<SG::Id>>().unwrap()) {
                        break;
                    }
                }
            });
        }
    }
}

/// A convenience type for getting access to [`TimingLayer`] through a `Dispatch`.
///
/// See [`TimingLayer::downcaster`].
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

impl<S, E> TimingLayer<S, E>
where
    S: SpanGroup,
    E: EventGroup,
    S::Id: Clone + Hash + Eq,
    E::Id: Clone + Hash + Eq,
{
    /// Returns an identifier that can later be used to get access to this [`TimingLayer`]
    /// after it has been turned into a `tracing::Dispatch`.
    ///
    /// ```rust
    /// use tracing::*;
    /// use tracing_timing::{Builder, Histogram, TimingLayer};
    /// use tracing_subscriber::{registry::Registry, Layer};
    /// let layer = Builder::default()
    ///     .layer(|| Histogram::new_with_max(1_000_000, 2).unwrap());
    /// let downcaster = layer.downcaster();
    /// let dispatch = Dispatch::new(layer.with_subscriber(Registry::default()));
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
    /// Retrieve a reference to this ident's original [`TimingLayer`].
    ///
    /// This method returns `None` if the given `Dispatch` is not holding a layer of the same
    /// type as this ident was created from.
    pub fn downcast<'a>(&self, d: &'a Dispatch) -> Option<&'a TimingLayer<S, E>> {
        d.downcast_ref()
    }
}
