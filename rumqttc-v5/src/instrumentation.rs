use crate::{ConnectionError, EventLoopDiagnostics, ProtocolViolation, StateError};

const PROTOCOL: &str = "v5";
const TARGET: &str = "rumqttc::lifecycle";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttemptContext {
    pub(crate) attempt_id: u64,
    pub(crate) connection_generation: u64,
    pub(crate) attempt_in_generation: u64,
}

impl AttemptContext {
    const fn reconnect(self) -> bool {
        self.connection_generation > 0
    }
}

#[derive(Debug, Default)]
pub struct ConnectionTelemetry {
    attempt_id: u64,
    connection_generation: u64,
    attempt_in_generation: u64,
    last_attempt: Option<AttemptContext>,
    connection_established: bool,
}

impl ConnectionTelemetry {
    pub(crate) const fn begin_attempt(&mut self) -> AttemptContext {
        self.attempt_id = self.attempt_id.saturating_add(1);
        self.attempt_in_generation = self.attempt_in_generation.saturating_add(1);
        let context = AttemptContext {
            attempt_id: self.attempt_id,
            connection_generation: self.connection_generation,
            attempt_in_generation: self.attempt_in_generation,
        };
        self.last_attempt = Some(context);
        context
    }

    pub(crate) const fn last_attempt(&self) -> Option<AttemptContext> {
        self.last_attempt
    }

    pub(crate) const fn connection_generation(&self) -> u64 {
        self.connection_generation
    }

    pub(crate) const fn mark_connection_established(&mut self) {
        self.connection_established = true;
    }

    pub(crate) const fn finish_established_connection(&mut self) -> bool {
        if !self.connection_established {
            return false;
        }

        self.connection_established = false;
        self.connection_generation = self.connection_generation.saturating_add(1);
        self.attempt_in_generation = 0;
        true
    }
}

pub fn connection_attempt(context: AttemptContext) {
    tracing::event!(
        name: "mqtt.connection_attempt",
        target: TARGET,
        tracing::Level::DEBUG,
        protocol = PROTOCOL,
        attempt_id = context.attempt_id,
        connection_generation = context.connection_generation,
        attempt_in_generation = context.attempt_in_generation,
        reconnect = context.reconnect(),
    );
}

pub fn connection_attempt_failed(
    context: AttemptContext,
    phase: &'static str,
    error: &ConnectionError,
) {
    tracing::event!(
        name: "mqtt.connection_attempt_failed",
        target: TARGET,
        tracing::Level::WARN,
        protocol = PROTOCOL,
        attempt_id = context.attempt_id,
        connection_generation = context.connection_generation,
        attempt_in_generation = context.attempt_in_generation,
        reconnect = context.reconnect(),
        phase,
        error_kind = connection_error_kind(error),
        error = %error,
    );
}

pub fn connection_established(context: AttemptContext, session_present: bool) {
    tracing::event!(
        name: "mqtt.connection_established",
        target: TARGET,
        tracing::Level::INFO,
        protocol = PROTOCOL,
        attempt_id = context.attempt_id,
        connection_generation = context.connection_generation,
        attempt_in_generation = context.attempt_in_generation,
        reconnect = context.reconnect(),
        session_present,
    );
}

pub fn connection_lost(
    context: Option<AttemptContext>,
    error: &ConnectionError,
    diagnostics: &EventLoopDiagnostics,
) {
    let context = context.unwrap_or(AttemptContext {
        attempt_id: 0,
        connection_generation: 0,
        attempt_in_generation: 0,
    });
    let queue_depth = diagnostics.queues.pending_len
        + diagnostics.queues.requests_rx_len
        + diagnostics.queues.control_requests_rx_len;
    tracing::event!(
        name: "mqtt.connection_lost",
        target: TARGET,
        tracing::Level::WARN,
        protocol = PROTOCOL,
        attempt_id = context.attempt_id,
        connection_generation = context.connection_generation,
        attempt_in_generation = context.attempt_in_generation,
        reconnect = context.reconnect(),
        inflight = diagnostics.outbound.inflight,
        inflight_limit = diagnostics.outbound.max_inflight,
        queue_depth,
        error_kind = connection_error_kind(error),
        error = %error,
    );
}

pub fn session_restored(connection_generation: u64, replay_count: usize) {
    tracing::event!(
        name: "mqtt.session_restored",
        target: TARGET,
        tracing::Level::INFO,
        protocol = PROTOCOL,
        connection_generation,
        reconnect = connection_generation > 0,
        replay_count,
    );
}

pub fn replay_prepared(connection_generation: u64, diagnostics: &EventLoopDiagnostics) {
    tracing::event!(
        name: "mqtt.replay_prepared",
        target: TARGET,
        tracing::Level::INFO,
        protocol = PROTOCOL,
        connection_generation,
        reconnect = connection_generation > 0,
        replay_count = diagnostics.queues.pending_replay_len,
        inflight = diagnostics.outbound.inflight,
        inflight_limit = diagnostics.outbound.max_inflight,
    );
}

pub fn protocol_violation(violation: &ProtocolViolation) {
    tracing::event!(
        name: "mqtt.protocol_violation",
        target: TARGET,
        tracing::Level::ERROR,
        protocol = PROTOCOL,
        violation_kind = protocol_violation_kind(violation),
        error = %violation,
    );
}

const fn connection_error_kind(error: &ConnectionError) -> &'static str {
    match error {
        ConnectionError::MqttState(StateError::Io(_)) | ConnectionError::Io(_) => "io",
        ConnectionError::MqttState(
            StateError::ProtocolViolation(_) | StateError::Deserialization(_),
        )
        | ConnectionError::NotConnAck(_)
        | ConnectionError::SessionStateMismatch { .. } => "protocol",
        ConnectionError::Timeout(_) => "timeout",
        ConnectionError::DisconnectTimeout => "disconnect_timeout",
        #[cfg(feature = "websocket")]
        ConnectionError::Websocket(_)
        | ConnectionError::WsConnect(_)
        | ConnectionError::ResponseValidation(_) => "websocket",
        #[cfg(any(feature = "use-rustls-no-provider", feature = "use-native-tls"))]
        ConnectionError::Tls(_) => "tls",
        ConnectionError::ConnectionRefused(_) => "refused",
        ConnectionError::SessionStore(_) => "session_store",
        ConnectionError::SessionRestore(_) => "session_restore",
        ConnectionError::BrokerTransportMismatch => "configuration",
        ConnectionError::RequestsDone => "requests_done",
        ConnectionError::AuthProcessingError => "authentication",
        #[cfg(feature = "websocket")]
        ConnectionError::InvalidUrl(_) | ConnectionError::RequestModifier(_) => "configuration",
        #[cfg(feature = "proxy")]
        ConnectionError::Proxy(_) => "proxy",
        ConnectionError::MqttState(_) => "state",
    }
}

const fn protocol_violation_kind(violation: &ProtocolViolation) -> &'static str {
    match violation {
        ProtocolViolation::DuplicateConnAck => "duplicate_connack",
        ProtocolViolation::UnexpectedIncomingPacket(_) => "unexpected_incoming_packet",
        ProtocolViolation::SubAckReturnCodeCountMismatch { .. } => {
            "suback_return_code_count_mismatch"
        }
        ProtocolViolation::UnsubAckReasonCodeCountMismatch { .. } => {
            "unsuback_reason_code_count_mismatch"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventLoop, MqttOptions};
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Value {
        Bool(bool),
        Debug(String),
        I64(i64),
        Str(String),
        U64(u64),
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        name: &'static str,
        target: &'static str,
        fields: BTreeMap<String, Value>,
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<CapturedEvent>>>);

    impl<S> Layer<S> for Capture
    where
        S: Subscriber,
    {
        fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
            Interest::always()
        }

        fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
            if event.metadata().target() != TARGET {
                return;
            }

            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            self.0.lock().unwrap().push(CapturedEvent {
                name: event.metadata().name(),
                target: event.metadata().target(),
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

    #[test]
    fn lifecycle_schema_uses_stable_names_and_typed_fields() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::set_global_default(subscriber)
            .expect("schema test is the only global tracing subscriber");

        let mut telemetry = ConnectionTelemetry::default();
        let attempt = telemetry.begin_attempt();
        connection_attempt(attempt);
        connection_attempt_failed(
            attempt,
            "target_setup",
            &ConnectionError::BrokerTransportMismatch,
        );
        connection_established(attempt, true);
        session_restored(0, 3);

        let eventloop = EventLoop::new(MqttOptions::new("schema", "localhost"), 1);
        let diagnostics = eventloop.diagnostics();
        connection_lost(Some(attempt), &ConnectionError::RequestsDone, &diagnostics);
        replay_prepared(0, &diagnostics);
        protocol_violation(&ProtocolViolation::DuplicateConnAck);

        let events = capture.0.lock().unwrap();
        assert!(events.iter().all(|event| event.target == TARGET));
        let find = |name| {
            events
                .iter()
                .find(|event| event.name == name)
                .unwrap_or_else(|| panic!("missing {name} event"))
        };
        let attempt_event = find("mqtt.connection_attempt");
        assert_eq!(
            attempt_event.fields.get("protocol"),
            Some(&Value::Str("v5".to_owned()))
        );
        assert!(matches!(
            attempt_event.fields.get("attempt_id"),
            Some(Value::U64(_))
        ));
        assert!(matches!(
            attempt_event.fields.get("reconnect"),
            Some(Value::Bool(_))
        ));
        let failed_event = find("mqtt.connection_attempt_failed");
        assert_eq!(
            failed_event.fields.get("phase"),
            Some(&Value::Str("target_setup".to_owned()))
        );
        assert_eq!(
            failed_event.fields.get("error_kind"),
            Some(&Value::Str("configuration".to_owned()))
        );
        assert_eq!(
            find("mqtt.session_restored").fields.get("replay_count"),
            Some(&Value::U64(3))
        );
        assert!(matches!(
            find("mqtt.connection_lost").fields.get("inflight"),
            Some(Value::U64(_))
        ));
        assert_eq!(
            find("mqtt.protocol_violation").fields.get("violation_kind"),
            Some(&Value::Str("duplicate_connack".to_owned()))
        );
        assert!(matches!(
            find("mqtt.replay_prepared").fields.get("replay_count"),
            Some(Value::U64(_))
        ));
    }

    #[test]
    fn attempt_ids_advance_across_cancelled_attempts_and_generations() {
        let mut telemetry = ConnectionTelemetry::default();
        let first = telemetry.begin_attempt();
        let second = telemetry.begin_attempt();
        assert_eq!((first.attempt_id, first.attempt_in_generation), (1, 1));
        assert_eq!((second.attempt_id, second.attempt_in_generation), (2, 2));

        telemetry.mark_connection_established();
        assert!(telemetry.finish_established_connection());
        let reconnect = telemetry.begin_attempt();
        assert_eq!(reconnect.attempt_id, 3);
        assert_eq!(reconnect.connection_generation, 1);
        assert_eq!(reconnect.attempt_in_generation, 1);
        assert!(reconnect.reconnect());
    }

    #[test]
    fn generation_only_advances_for_an_established_connection() {
        let mut telemetry = ConnectionTelemetry::default();
        assert!(!telemetry.finish_established_connection());
        assert_eq!(telemetry.connection_generation(), 0);

        telemetry.begin_attempt();
        telemetry.mark_connection_established();
        assert!(telemetry.finish_established_connection());
        assert!(!telemetry.finish_established_connection());
        assert_eq!(telemetry.connection_generation(), 1);
    }
}
