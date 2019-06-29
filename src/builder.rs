use crate::{group, EventGroup, Histogram, SpanGroup, TimingSubscriber};
use std::hash::Hash;

/// Builder for [`TimingSubscriber`] instances.
///
/// This type implements the [builder pattern]. It lets you easily configure and construct a new
/// [`TimingSubscriber`] subscriber. See the individual methods for details. To start, use
/// [`Builder::from`]:
///
/// ```rust
/// use tracing_timing::{Builder, Histogram};
/// let builder = Builder::default();
/// let subscriber = builder.build(|| Histogram::new(3).unwrap());
/// ```
///
/// See the various `new_*` methods on [`Histogram`] for how to construct an appropriate histogram
/// in the first place. All samples recorded by the subscriber returned from [`Builder::build`]
/// will be recorded into histograms as returned by the provided constructor. You can also
/// construct the histograms based on the span and event group it will be tracking by using
/// [`Builder::build_informed`].
///
///   [builder pattern]: https://rust-lang-nursery.github.io/api-guidelines/type-safety.html#c-builder
pub struct Builder<S = group::ByName, E = group::ByMessage> {
    span_group: S,
    event_group: E,
    time: quanta::Clock,
}

impl Default for Builder<group::ByName, group::ByMessage> {
    fn default() -> Self {
        Builder {
            span_group: group::ByName,
            event_group: group::ByMessage,
            time: quanta::Clock::new(),
        }
    }
}

impl<S, E> Builder<S, E> {
    /// Set the mechanism used to divide spans into groups.
    ///
    /// See [`SpanGroup`] and the [`group`] module for details.
    pub fn spans<S2>(self, span_group: S2) -> Builder<S2, E> {
        Builder {
            span_group,
            event_group: self.event_group,
            time: self.time,
        }
    }

    /// Set the mechanism used to divide events into per-span groups.
    ///
    /// See [`EventGroup`] and the [`group`] module for details.
    pub fn events<E2>(self, event_group: E2) -> Builder<S, E2> {
        Builder {
            span_group: self.span_group,
            event_group,
            time: self.time,
        }
    }

    /// Set the time source to use for time measurements.
    pub fn time(mut self, time: quanta::Clock) -> Builder<S, E> {
        self.time = time;
        self
    }

    /// Construct a [`TimingSubscriber`] that uses the given function to construct new histograms.
    ///
    /// This is equivalent to [`build`], except that the passed function is also told which
    /// span/event group the histogram is for.
    ///
    /// Note that you _may_ run into weird lifetime errors from the compiler when using this method
    /// with a closure. This is a [known compiler issue]. You can work around it by adding a slight
    /// type hint to the arguments passed to the closure as follows (note the `: &_`):
    ///
    /// ```rust
    /// use tracing_timing::{Builder, Histogram};
    /// let builder = Builder::default();
    /// let subscriber = builder.build_informed(|s: &_, e: &_| Histogram::new(3).unwrap());
    /// ```
    ///
    ///   [known compiler issue]: https://github.com/rust-lang/rust/issues/41078
    pub fn build_informed<F>(self, new_histogram: F) -> TimingSubscriber<S, E>
    where
        S: SpanGroup,
        E: EventGroup,
        S::Id: Hash + Eq,
        E::Id: Hash + Eq,
        F: FnMut(&S::Id, &E::Id) -> Histogram<u64> + Send + Sync + 'static,
    {
        let (tx, rx) = crossbeam::channel::unbounded();
        TimingSubscriber {
            span_group: self.span_group,
            event_group: self.event_group,
            time: self.time,
            reader: super::ReaderState {
                created: rx,
                histograms: Default::default(),
            }
            .into(),
            writers: super::WriterState {
                last_event: Default::default(),
                refcount: Default::default(),
                spans: Default::default(),
                tls: Default::default(),
                idle_recorders: Default::default(),
                new_histogram: Box::new(new_histogram),
                created: tx,
            }
            .into(),
        }
    }

    /// Construct a [`TimingSubscriber`] that uses the given function to construct new histograms.
    pub fn build<F>(self, mut new_histogram: F) -> TimingSubscriber<S, E>
    where
        S: SpanGroup,
        E: EventGroup,
        S::Id: Hash + Eq,
        E::Id: Hash + Eq,
        F: FnMut() -> Histogram<u64> + Send + Sync + 'static,
    {
        self.build_informed(move |_: &_, _: &_| (new_histogram)())
    }
}
