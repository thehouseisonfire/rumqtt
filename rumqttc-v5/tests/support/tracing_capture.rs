use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub const LIFECYCLE_TARGET: &str = "rumqttc::lifecycle";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Debug(String),
    I64(i64),
    Str(String),
    U64(u64),
}

#[derive(Clone, Debug)]
pub struct CapturedEvent {
    pub name: &'static str,
    pub target: &'static str,
    pub level: Level,
    pub fields: BTreeMap<String, Value>,
}

impl CapturedEvent {
    pub fn field(&self, name: &str) -> &Value {
        self.fields
            .get(name)
            .unwrap_or_else(|| panic!("event {} is missing field {name}", self.name))
    }

    pub fn assert_metadata(&self, name: &str, level: Level) {
        assert_eq!(self.name, name);
        assert_eq!(self.target, LIFECYCLE_TARGET);
        assert_eq!(self.level, level);
    }
}

#[derive(Clone, Default)]
pub struct Capture(Arc<Mutex<Vec<CapturedEvent>>>);

impl Capture {
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.0.lock().unwrap().clone()
    }
}

impl<S> Layer<S> for Capture
where
    S: Subscriber,
{
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::always()
    }

    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() != LIFECYCLE_TARGET {
            return;
        }

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.0.lock().unwrap().push(CapturedEvent {
            name: event.metadata().name(),
            target: event.metadata().target(),
            level: *event.metadata().level(),
            fields: visitor.fields,
        });
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, Value>,
}

impl Visit for FieldVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), Value::Bool(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), Value::Debug(format!("{value:?}")));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), Value::I64(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), Value::Str(value.to_owned()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), Value::U64(value));
    }
}

pub fn only_event<'a>(events: &'a [CapturedEvent], name: &str) -> &'a CapturedEvent {
    let matching = events
        .iter()
        .filter(|event| event.name == name)
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1, "expected exactly one {name} event");
    matching[0]
}

pub fn assert_no_event(events: &[CapturedEvent], name: &str) {
    assert!(
        events.iter().all(|event| event.name != name),
        "unexpected {name} event"
    );
}

pub fn event_position(events: &[CapturedEvent], name: &str) -> usize {
    events
        .iter()
        .position(|event| event.name == name)
        .unwrap_or_else(|| panic!("missing {name} event"))
}
