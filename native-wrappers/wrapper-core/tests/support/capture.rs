#[cfg(feature = "tracing")]
mod enabled {
    use std::fmt::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use tracing::{
        Event, Metadata, Subscriber,
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };

    struct Capture {
        output: Arc<Mutex<String>>,
        next: AtomicU64,
    }

    struct LogCapture(Arc<Mutex<String>>);
    impl log::Log for LogCapture {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.target().starts_with("rumqttc")
                || metadata.target().starts_with("mqtt.wrapper")
        }
        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                writeln!(self.0.lock().unwrap(), "{}", record.args()).unwrap();
            }
        }
        fn flush(&self) {}
    }
    struct Fields<'a>(&'a mut String);
    impl Visit for Fields<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            write!(self.0, " {}={value:?}", field.name()).unwrap();
        }
    }
    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attributes: &Attributes<'_>) -> Id {
            let mut output = self.output.lock().unwrap();
            output.push_str(attributes.metadata().name());
            attributes.record(&mut Fields(&mut output));
            drop(output);
            Id::from_u64(self.next.fetch_add(1, Ordering::Relaxed) + 1)
        }
        fn record(&self, _: &Id, values: &Record<'_>) {
            values.record(&mut Fields(&mut self.output.lock().unwrap()));
        }
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            event.record(&mut Fields(&mut self.output.lock().unwrap()));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }
    pub fn output() -> &'static Arc<Mutex<String>> {
        static OUTPUT: OnceLock<Arc<Mutex<String>>> = OnceLock::new();
        OUTPUT.get_or_init(|| {
            let output = Arc::new(Mutex::new(String::new()));
            if cfg!(feature = "tracing-log-compat") {
                // Exercise the actual fallback with no tracing subscriber.
                log::set_boxed_logger(Box::new(LogCapture(output.clone()))).unwrap();
                log::set_max_level(log::LevelFilter::Trace);
            } else {
                tracing::subscriber::set_global_default(Capture {
                    output: output.clone(),
                    next: AtomicU64::new(0),
                })
                .unwrap();
            }
            output
        })
    }
}

pub fn start() {
    #[cfg(feature = "tracing")]
    let _ = enabled::output();
}

pub fn assert_activity() {
    #[cfg(feature = "tracing")]
    assert!(
        !enabled::output().lock().unwrap().is_empty(),
        "no lifecycle output was captured"
    );
}

pub fn assert_redacted(debug_and_errors: &str, secrets: &[&str]) {
    #[cfg(feature = "tracing")]
    let traces = enabled::output().lock().unwrap().clone();
    #[cfg(not(feature = "tracing"))]
    let traces = String::new();
    for secret in secrets {
        assert!(!secret.is_empty());
        assert!(
            !debug_and_errors.contains(secret),
            "secret appeared in formatted output"
        );
        assert!(
            !traces.contains(secret),
            "secret appeared in tracing output"
        );
    }
}
