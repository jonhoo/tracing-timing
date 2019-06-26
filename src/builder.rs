use crate::{group, EventGroup, Histogram, Sampler, SpanGroup};
use std::hash::Hash;

/// Builder for [`Sampler`] instances.
///
/// This type implements the [builder pattern]. It lets you easily configure and construct a new
/// [`Sampler`] subscriber. See the individual methods for details. To start, use
/// [`Builder::from`]:
///
/// ```rust
/// use tracing_metrics::{Builder, Histogram};
/// let builder = Builder::from(|| Histogram::new(3).unwrap());
/// let subscriber = builder.build();
/// ```
///
/// See the various `new_*` methods on [`Histogram`] for how to construct an appropriate histogram
/// in the first place. All samples recorded by the subscriber returned from [`Builder::build`]
/// will be recorded into histograms as returned by the provided constructor.
///
///   [builder pattern]: https://rust-lang-nursery.github.io/api-guidelines/type-safety.html#c-builder
pub struct Builder<NH, S = group::ByName, E = group::ByMessage> {
    span_group: S,
    event_group: E,
    time: quanta::Clock,
    new_histogram: NH,
}

impl<NH> From<NH> for Builder<NH, group::ByName, group::ByMessage> {
    fn from(new_histogram: NH) -> Self {
        Builder {
            span_group: Default::default(),
            event_group: Default::default(),
            time: quanta::Clock::new(),
            new_histogram,
        }
    }
}

impl<NH, S, E> Builder<NH, S, E> {
    /// Set the mechanism used to divide spans into groups.
    ///
    /// See [`SpanGroup`] and the [`group`] module for details.
    pub fn spans<S2>(self, span_group: S2) -> Builder<NH, S2, E> {
        Builder {
            span_group,
            event_group: self.event_group,
            time: self.time,
            new_histogram: self.new_histogram,
        }
    }

    /// Set the mechanism used to divide events into per-span groups.
    ///
    /// See [`EventGroup`] and the [`group`] module for details.
    pub fn events<E2>(self, event_group: E2) -> Builder<NH, S, E2> {
        Builder {
            span_group: self.span_group,
            event_group,
            time: self.time,
            new_histogram: self.new_histogram,
        }
    }

    /// Set the time source to use for time measurements.
    pub fn time(mut self, time: quanta::Clock) -> Builder<NH, S, E> {
        self.time = time;
        self
    }

    /// Construct a [`Sampler`] as configured.
    pub fn build(self) -> Sampler<NH, S, E>
    where
        S: SpanGroup,
        E: EventGroup,
        S::Id: Hash + Eq,
        E::Id: Hash + Eq,
        NH: FnMut() -> Histogram<u64>,
    {
        Sampler {
            span_group: self.span_group,
            event_group: self.event_group,
            time: self.time,
            histograms: super::MasterHistograms::from(self.new_histogram).into(),
            shared: Default::default(),
        }
    }
}
