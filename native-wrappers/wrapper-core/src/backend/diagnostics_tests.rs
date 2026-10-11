use super::*;
use crate::NativeDiagnosticsSnapshot;

fn handle(mqtt5: bool) -> ClientHandle {
    let backend = if mqtt5 {
        let (tx, _rx) = flume::bounded(1);
        BackendClient::V5(rumqttc_v5::AsyncClient::from_senders(tx))
    } else {
        let (tx, _rx) = flume::bounded(1);
        BackendClient::V4(rumqttc_v4::AsyncClient::from_senders(tx))
    };
    let (operations, _receivers) = OperationRegistry::new(1);
    let acknowledgements = AcknowledgementCoordinator::new(1, operations.clone());
    let (immediate, _) = flume::bounded(1);
    let shutdown = ShutdownCoordinator::new(operations.clone(), immediate);
    let (panic, _) = flume::bounded(1);
    let shared = Shared::new(
        (
            backend,
            rumqttc_core::session_recovery::SessionRecoveryGate::new(
                "measurement".into(),
                "measurement".into(),
            ),
        ),
        acknowledgements,
        ConnectionHandle::new(),
        operations,
        shutdown,
        panic,
        crate::reconnect::Controller::new(crate::ReconnectPolicy::Legacy),
    );
    let configuration = crate::configuration_update::ConfigurationDriver::new(
        if mqtt5 {
            crate::ClientConfig::v5("measurement", "localhost", 1883)
        } else {
            crate::ClientConfig::v4("measurement", "localhost", 1883)
        },
        &rumqttc_core::ConnectionObservation::default(),
        Arc::default(),
    );
    shared.set_configuration(configuration.control.clone());
    ClientHandle::new(shared)
}

#[test]
fn publication_keeps_only_the_latest_capture_and_caller_retention_is_independent() {
    let handle = handle(true);
    let (_, eventloop) = rumqttc_v5::AsyncClient::builder(rumqttc_v5::MqttOptions::new(
        "retention",
        ("localhost", 1883),
    ))
    .build();
    handle
        .shared
        .publish_native_diagnostics(NativeDiagnosticsSnapshot::v5(eventloop.diagnostics()));
    let retained = handle.diagnostics_snapshot();
    let first = Arc::downgrade(retained.native.as_ref().unwrap());
    for generation in 2..=1000 {
        let previous = handle.shared.cached_native_diagnostics().unwrap();
        let weak = Arc::downgrade(&previous);
        drop(previous);
        handle
            .shared
            .publish_native_diagnostics(NativeDiagnosticsSnapshot::v5(eventloop.diagnostics()));
        assert_eq!(
            handle.diagnostics_snapshot().native.unwrap().generation,
            generation
        );
        if generation > 2 {
            assert!(weak.upgrade().is_none());
        }
    }
    assert!(first.upgrade().is_some());
    drop(retained);
    assert!(first.upgrade().is_none());
    let mut cache = handle.shared.diagnostics.lock().unwrap();
    Arc::get_mut(cache.as_mut().unwrap()).unwrap().generation = u64::MAX;
    drop(cache);
    handle
        .shared
        .publish_native_diagnostics(NativeDiagnosticsSnapshot::v5(eventloop.diagnostics()));
    assert_eq!(
        handle.diagnostics_snapshot().native.unwrap().generation,
        u64::MAX
    );
}

#[test]
#[ignore = "manual release-mode diagnostics cost measurement"]
fn diagnostics_cost_measurements() {
    use std::hint::black_box;
    use std::time::Instant;
    fn measure(mut work: impl FnMut()) -> (u128, u128, u128) {
        const ITERATIONS: u32 = 2000;
        for _ in 0..100 {
            work();
        }
        let mut samples = [0; 7];
        for sample in &mut samples {
            let began = Instant::now();
            for _ in 0..ITERATIONS {
                work();
            }
            *sample = began.elapsed().as_nanos() / u128::from(ITERATIONS);
        }
        samples.sort_unstable();
        (samples[0], samples[3], samples[6])
    }
    println!("protocol,inflight,operation,min_ns,median_ns,max_ns");
    for limit in [16, 4096] {
        let mut options = rumqttc_v4::MqttOptions::new("cost", ("localhost", 1883));
        options.set_inflight(limit);
        let (_, mut v4) = rumqttc_v4::AsyncClient::builder(options).build();
        let mut options = rumqttc_v5::MqttOptions::new("cost", ("localhost", 1883));
        options.set_outgoing_inflight_upper_limit(limit);
        let (_, mut v5) = rumqttc_v5::AsyncClient::builder(options).build();
        for _ in 0..limit {
            v4.state
                .handle_outgoing_packet(rumqttc_v4::Request::Publish(rumqttc_v4::Publish::new(
                    "a",
                    rumqttc_v4::QoS::AtLeastOnce,
                    "payload",
                )))
                .unwrap();
            v5.state
                .handle_outgoing_packet(rumqttc_v5::Request::Publish(rumqttc_v5::Publish::new(
                    "a",
                    rumqttc_v5::QoS::AtLeastOnce,
                    "payload",
                    None,
                )))
                .unwrap();
        }
        for protocol in [4, 5] {
            let handle = handle(protocol == 5);
            assert_eq!(v4.diagnostics().outbound.inflight, limit);
            assert_eq!(v5.diagnostics().outbound.inflight, limit);
            let baseline = measure(|| {
                if protocol == 4 {
                    black_box(black_box(&v4).diagnostics());
                } else {
                    black_box(black_box(&v5).diagnostics());
                }
            });
            let capture = measure(|| {
                let native = if protocol == 4 {
                    NativeDiagnosticsSnapshot::v4(&black_box(&v4).diagnostics())
                } else {
                    NativeDiagnosticsSnapshot::v5(black_box(&v5).diagnostics())
                };
                black_box(native.legacy());
                handle.shared.publish_native_diagnostics(native);
            });
            let read = measure(|| {
                black_box(handle.diagnostics_snapshot());
            });
            for (operation, (min, median, max)) in [
                ("native_baseline", baseline),
                ("capture_projection_publication", capture),
                ("owned_acquisition", read),
            ] {
                println!("{protocol},{limit},{operation},{min},{median},{max}");
            }
        }
    }
    println!(
        "native_capture_bytes={}",
        std::mem::size_of::<NativeDiagnosticsSnapshot>()
    );
}
