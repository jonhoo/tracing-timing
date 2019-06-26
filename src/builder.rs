use crate::{group, EventGroup, Sampler, SpanGroup};
use hdrhistogram::Histogram;
use std::hash::Hash;

pub struct Builder<NH, S = group::ByName, E = group::ByTarget> {
    span_group: S,
    event_group: E,
    time: quanta::Clock,
    new_histogram: NH,
}

impl<NH> From<NH> for Builder<NH, group::ByName, group::ByTarget> {
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
    pub fn spans<S2>(self, span_group: S2) -> Builder<NH, S2, E> {
        Builder {
            span_group,
            event_group: self.event_group,
            time: self.time,
            new_histogram: self.new_histogram,
        }
    }

    pub fn events<E2>(self, event_group: E2) -> Builder<NH, S, E2> {
        Builder {
            span_group: self.span_group,
            event_group,
            time: self.time,
            new_histogram: self.new_histogram,
        }
    }

    pub fn time(mut self, time: quanta::Clock) -> Builder<NH, S, E> {
        self.time = time;
        self
    }

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
