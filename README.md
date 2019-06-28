[![Crates.io](https://img.shields.io/crates/v/tracing-timing.svg)](https://crates.io/crates/tracing-timing)
[![Documentation](https://docs.rs/tracing-timing/badge.svg)](https://docs.rs/tracing-timing/)
[![Travis Build Status](https://travis-ci.com/jonhoo/tracing-timing.svg?branch=master)](https://travis-ci.com/jonhoo/tracing-timing)
[![Cirrus CI Build Status](https://api.cirrus-ci.com/github/jonhoo/tracing-timing.svg)](https://cirrus-ci.com/github/jonhoo/tracing-timing)
[![Codecov](https://codecov.io/github/jonhoo/tracing-timing/coverage.svg?branch=master)](https://codecov.io/gh/jonhoo/tracing-timing)
[![Dependency status](https://deps.rs/repo/github/jonhoo/tracing-timing/status.svg)](https://deps.rs/repo/github/jonhoo/tracing-timing)

Inter-event timing metrics on top of [`tracing`].

This crate provides a `tracing::Subscriber` that keeps statistics on
inter-event timing information. More concretely, given code like this:

```rust
use tracing::*;
use tracing_timing::{Builder, Histogram};
let subscriber = Builder::from(|| Histogram::new_with_max(1_000_000, 2).unwrap()).build();
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

You can produce something like this:

```text
fast:
  50µs |
 100µs |
 150µs | ****
 200µs | ************
 250µs | *************
 300µs | *****
 350µs | ****
 400µs | *
 450µs |
 500µs |

slow:
 550µs |
 600µs |
 650µs | ******
 700µs | **********************
 750µs | *********
 800µs | **
 850µs | *
 900µs |
```

When `TimingSubscriber` is used as the `tracing::Dispatch`, the time
between each event in a span is measured using [`quanta`], and is
recorded in "[high dynamic range histograms]" using [`hdrhistogram`]'s
multi-threaded recording facilities. The recorded timing information is
grouped using the `SpanGroup` and `EventGroup` traits, allowing you to
combine recorded statistics across spans and events.

  [`tracing`]: https://docs.rs/tracing-core/
  [high dynamic range histograms]: https://hdrhistogram.github.io/HdrHistogram/
  [`hdrhistogram`]: https://docs.rs/hdrhistogram/
  [`quanta`]: https://docs.rs/quanta/
