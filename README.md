[![Crates.io](https://img.shields.io/crates/v/tracing-timing.svg)](https://crates.io/crates/tracing-timing)
[![Documentation](https://docs.rs/tracing-timing/badge.svg)](https://docs.rs/tracing-timing/)
[![Codecov](https://codecov.io/github/jonhoo/tracing-timing/coverage.svg?branch=master)](https://codecov.io/gh/jonhoo/tracing-timing)
[![Dependency status](https://deps.rs/repo/github/jonhoo/tracing-timing/status.svg)](https://deps.rs/repo/github/jonhoo/tracing-timing)

Inter-event timing metrics on top of [`tracing`].

This crate provides a `tracing::Subscriber` that keeps statistics on inter-event timing
information. More concretely, given code like this:

```rust
use tracing::*;
use tracing_timing::{Builder, Histogram};
let subscriber = Builder::default().build(|| Histogram::new_with_max(1_000_000, 2).unwrap());
let dispatcher = Dispatch::new(subscriber);
dispatcher::with_default(&dispatcher, || {
    trace_span!("request").in_scope(|| {
        // do a little bit of work
        trace!("fast");
        // do a lot of work
        trace!("slow");
    })
});
```

You can produce something like this (see `examples/pretty.rs`):

```text
fast:
mean: 173.2µs, p50: 172µs, p90: 262µs, p99: 327µs, p999: 450µs, max: 778µs
  25µs | *                                        |  2.2th %-ile
  50µs | *                                        |  2.2th %-ile
  75µs | *                                        |  4.7th %-ile
 100µs | ***                                      | 11.5th %-ile
 125µs | *****                                    | 24.0th %-ile
 150µs | *******                                  | 41.1th %-ile
 175µs | ********                                 | 59.2th %-ile
 200µs | *******                                  | 75.4th %-ile
 225µs | **                                       | 80.1th %-ile
 250µs | ***                                      | 87.3th %-ile
 275µs | ***                                      | 94.4th %-ile
 300µs | **                                       | 97.8th %-ile

slow:
mean: 623.3µs, p50: 630µs, p90: 696µs, p99: 770µs, p999: 851µs, max: 950µs
 500µs | *                                        |  1.6th %-ile
 525µs | **                                       |  4.8th %-ile
 550µs | ***                                      | 10.9th %-ile
 575µs | *****                                    | 22.2th %-ile
 600µs | *******                                  | 37.9th %-ile
 625µs | ********                                 | 55.9th %-ile
 650µs | *******                                  | 72.9th %-ile
 675µs | ******                                   | 85.6th %-ile
 700µs | ****                                     | 93.5th %-ile
 725µs | **                                       | 97.1th %-ile
```

When [`TimingSubscriber`] is used as the `tracing::Dispatch`, the time between each event in a
span is measured using [`quanta`], and is recorded in "[high dynamic range histograms]" using
[`hdrhistogram`]'s multi-threaded recording facilities. The recorded timing information is
grouped using the [`SpanGroup`] and [`EventGroup`] traits, allowing you to combine recorded
statistics across spans and events.

  [`tracing`]: https://docs.rs/tracing-core/
  [high dynamic range histograms]: https://hdrhistogram.github.io/HdrHistogram/
  [`hdrhistogram`]: https://docs.rs/hdrhistogram/
  [`quanta`]: https://docs.rs/quanta/
