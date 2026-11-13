//! [`LineLog`]: the `tracing` subscriber of `alloyd`, one line per event.

use std::fmt::{self, Write as _};
use std::io::Write;
use std::sync::{Mutex, PoisonError};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// A `tracing` subscriber that writes each event at `INFO` or above as one
/// line: the level, the message, then each other field as `name=value`, as in
/// `INFO alloyd started version=0.0.0 data_dir=/var/lib/alloyd`.
///
/// A value that is empty or holds whitespace, a quote, `=`, or a control
/// character is written quoted and escaped, so that every event is exactly one
/// line. The line carries no time: the service manager's log adds it. Spans
/// are accepted and not written. A failed write is dropped, since a daemon has
/// nowhere else to report it.
pub struct LineLog<W> {
    out: Mutex<W>,
}

impl<W> fmt::Debug for LineLog<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineLog").finish_non_exhaustive()
    }
}

impl<W: Write + Send + 'static> LineLog<W> {
    /// A subscriber writing to `out`, typically standard error.
    pub fn new(out: W) -> LineLog<W> {
        LineLog {
            out: Mutex::new(out),
        }
    }
}

impl<W: Write + Send + 'static> Subscriber for LineLog<W> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::INFO
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::INFO)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = Line::default();
        event.record(&mut line);
        let mut text = event.metadata().level().as_str().to_owned();
        if !line.message.is_empty() {
            text.push(' ');
            text.push_str(&line.message);
        }
        text.push_str(&line.fields);
        text.push('\n');
        let mut out = self.out.lock().unwrap_or_else(PoisonError::into_inner);
        // Nowhere to report a failed write: see the type's documentation.
        let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// The parts of one line, collected from an event's fields.
#[derive(Default)]
struct Line {
    message: String,
    fields: String,
}

impl Visit for Line {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, &format!("{value:?}"));
    }
}

impl Line {
    fn push(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.clear();
            for c in value.chars() {
                if c.is_control() {
                    self.message.extend(c.escape_default());
                } else {
                    self.message.push(c);
                }
            }
            return;
        }
        let plain = !value.is_empty()
            && !value
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=');
        if plain {
            let _ = write!(self.fields, " {}={value}", field.name());
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracing::{debug, error, info, info_span, warn};

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn log(body: impl FnOnce()) -> String {
        let sink = Sink::default();
        tracing::subscriber::with_default(LineLog::new(sink.clone()), body);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn events_are_single_lines_with_fields() {
        let text = log(|| {
            info!(version = "0.0.0", data_dir = %"/var/lib/alloyd", "alloyd started");
            warn!(reason = "two words", "careful");
            error!(error = %"a \"quoted\"\nline", count = 3, "failed");
            info!(empty = "", "with an empty value");
        });
        assert_eq!(
            text,
            "INFO alloyd started version=0.0.0 data_dir=/var/lib/alloyd\n\
             WARN careful reason=\"two words\"\n\
             ERROR failed error=\"a \\\"quoted\\\"\\nline\" count=3\n\
             INFO with an empty value empty=\"\"\n"
        );
    }

    #[test]
    fn debug_events_and_spans_are_not_written() {
        let text = log(|| {
            debug!("hidden");
            let span = info_span!("work", id = 1);
            let _entered = span.enter();
            info!("shown");
        });
        assert_eq!(text, "INFO shown\n");
    }

    #[test]
    fn control_characters_in_a_message_are_escaped() {
        let text = log(|| info!("{}", "two\nlines"));
        assert_eq!(text, "INFO two\\nlines\n");
    }
}
