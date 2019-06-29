//! Commonly used span and event grouping mechanisms.

use super::EventGroup;
use super::SpanGroup;
use std::borrow::Cow;
use std::fmt::Write;
use tracing_core::*;

impl<F, R> SpanGroup for F
where
    F: Fn(&span::Attributes) -> R,
{
    type Id = R;
    fn group(&self, a: &span::Attributes) -> Self::Id {
        (self)(a)
    }
}

impl<F, R> EventGroup for F
where
    F: Fn(&Event) -> R,
{
    type Id = R;
    fn group(&self, e: &Event) -> Self::Id {
        (self)(e)
    }
}

/// Group spans/events by their configured `target`.
///
/// This is usually the module path where the span or event was created.
/// See `Metadata::target` in `tracing` for details.
#[derive(Default, Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByTarget;

impl SpanGroup for ByTarget {
    type Id = &'static str;
    fn group(&self, a: &span::Attributes) -> Self::Id {
        a.metadata().target()
    }
}

impl EventGroup for ByTarget {
    type Id = &'static str;
    fn group(&self, e: &Event) -> Self::Id {
        e.metadata().target()
    }
}

/// Group spans/events by their "name".
///
/// For spans, this is the string passed as the first argument to the various span tracing macros.
/// For events, this is usually the source file and line number where the event was created.
#[derive(Default, Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByName;

impl SpanGroup for ByName {
    type Id = &'static str;
    fn group(&self, a: &span::Attributes) -> Self::Id {
        a.metadata().name()
    }
}

impl EventGroup for ByName {
    type Id = &'static str;
    fn group(&self, e: &Event) -> Self::Id {
        e.metadata().name()
    }
}

/// Group events by their "message".
///
/// This is the string passed as the first argument to the various event tracing macros.
#[derive(Default, Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByMessage;

impl EventGroup for ByMessage {
    type Id = String;
    fn group(&self, e: &Event) -> Self::Id {
        EventGroup::group(&ByField(Cow::Borrowed("message")), e)
    }
}

/// Group spans/events by the value of a particular field.
///
/// If a field by the contained name is found, its recorded value (usually its `Debug` value) is
/// used as the grouping identifier. If the field is not found, an empty `String` is used.
#[derive(Default, Clone, Debug, Eq, PartialEq)]
pub struct ByField(pub Cow<'static, str>);

impl<T> From<T> for ByField
where
    T: Into<Cow<'static, str>>,
{
    fn from(t: T) -> Self {
        Self(t.into())
    }
}

struct ByFieldVisitor<'a> {
    field: &'a ByField,
    value: String,
}

impl<'a> field::Visit for ByFieldVisitor<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == &*self.field.0 {
            write!(&mut self.value, "{:?}", value).unwrap();
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == &*self.field.0 {
            write!(&mut self.value, "{}", value).unwrap();
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == &*self.field.0 {
            write!(&mut self.value, "{}", value).unwrap();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == &*self.field.0 {
            write!(&mut self.value, "{}", value).unwrap();
        }
    }
}

impl SpanGroup for ByField {
    type Id = String;
    fn group(&self, a: &span::Attributes) -> Self::Id {
        let mut visitor = ByFieldVisitor {
            field: self,
            value: String::new(),
        };
        a.record(&mut visitor);
        visitor.value
    }
}

impl EventGroup for ByField {
    type Id = String;
    fn group(&self, e: &Event) -> Self::Id {
        let mut visitor = ByFieldVisitor {
            field: self,
            value: String::new(),
        };
        e.record(&mut visitor);
        visitor.value
    }
}
