use crate::{group, EventGroup, Sampler, SpanGroup};
use crossbeam::sync::ShardedLock;
use hdrhistogram::{sync::SyncHistogram, Histogram};
use std::hash::Hash;

pub struct Builder<S = group::ByName, E = group::ByTarget> {
    span_group: S,
    event_group: E,
    time: quanta::Clock,
    histogram: SyncHistogram<u64>,
}

impl From<Histogram<u64>> for Builder<group::ByName, group::ByTarget> {
    fn from(h: Histogram<u64>) -> Self {
        Builder {
            span_group: Default::default(),
            event_group: Default::default(),
            time: quanta::Clock::new(),
            histogram: h.into(),
        }
    }
}

impl<S, E> Builder<S, E> {
    pub fn spans<S2>(self, span_group: S2) -> Builder<S2, E> {
        Builder {
            span_group,
            event_group: self.event_group,
            time: self.time,
            histogram: self.histogram,
        }
    }

    pub fn events<E2>(self, event_group: E2) -> Builder<S, E2> {
        Builder {
            span_group: self.span_group,
            event_group,
            time: self.time,
            histogram: self.histogram,
        }
    }

    pub fn time(mut self, time: quanta::Clock) -> Builder<S, E> {
        self.time = time;
        self
    }

    pub fn build(self) -> Sampler<S, E>
    where
        S: SpanGroup,
        E: EventGroup,
        S::Id: Hash + Eq,
        E::Id: Hash + Eq,
    {
        Sampler {
            span_group: self.span_group,
            event_group: self.event_group,
            time: self.time,
            histogram: self.histogram,
            shared: ShardedLock::default(),
        }
    }
}
