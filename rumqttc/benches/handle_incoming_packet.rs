use std::time::Duration;

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use rumqttc::mqttbytes::QoS as V4QoS;
use rumqttc::mqttbytes::v4::Publish as V4Publish;
use rumqttc::v5::mqttbytes::QoS as V5QoS;
use rumqttc::v5::mqttbytes::v5::Publish as V5Publish;
use rumqttc::v5::{Incoming as V5Incoming, MqttState as V5State};
use rumqttc::{Incoming as V4Incoming, MqttState as V4State};

fn build_v4_publish(qos: V4QoS, pkid: u16) -> V4Publish {
    let mut publish = V4Publish::new(
        "bench/topic",
        V4QoS::AtLeastOnce,
        vec![1, 2, 3, 4, 5, 6, 7, 8],
    );
    publish.qos = qos;
    publish.pkid = pkid;
    publish
}

fn build_v5_publish(qos: V5QoS, pkid: u16) -> V5Publish {
    let mut publish = V5Publish::new(
        "bench/topic",
        V5QoS::AtLeastOnce,
        vec![1, 2, 3, 4, 5, 6, 7, 8],
        None,
    );
    publish.qos = qos;
    publish.pkid = pkid;
    publish
}

fn bench_v4(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_incoming_packet/v4");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(100);
    group.throughput(Throughput::Elements(1));

    group.bench_function("publish_qos0", |b| {
        let mut state = V4State::new(100, false);
        let packet = V4Incoming::Publish(build_v4_publish(V4QoS::AtMostOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.bench_function("publish_qos1_autoack", |b| {
        let mut state = V4State::new(100, false);
        let packet = V4Incoming::Publish(build_v4_publish(V4QoS::AtLeastOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.bench_function("publish_qos2_autoack", |b| {
        let mut state = V4State::new(100, false);
        let packet = V4Incoming::Publish(build_v4_publish(V4QoS::ExactlyOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.finish();
}

fn bench_v5(c: &mut Criterion) {
    let mut group = c.benchmark_group("handle_incoming_packet/v5");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(100);
    group.throughput(Throughput::Elements(1));

    group.bench_function("publish_qos0", |b| {
        let mut state = V5State::new(100, false, None);
        let packet = V5Incoming::Publish(build_v5_publish(V5QoS::AtMostOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.bench_function("publish_qos1_autoack", |b| {
        let mut state = V5State::new(100, false, None);
        let packet = V5Incoming::Publish(build_v5_publish(V5QoS::AtLeastOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.bench_function("publish_qos2_autoack", |b| {
        let mut state = V5State::new(100, false, None);
        let packet = V5Incoming::Publish(build_v5_publish(V5QoS::ExactlyOnce, 1));

        b.iter(|| {
            let outgoing = state
                .handle_incoming_packet(black_box(packet.clone()))
                .unwrap();
            black_box(outgoing);
            black_box(state.events.pop_front());
            state.events.clear();
        });
    });

    group.finish();
}

criterion_group!(benches, bench_v4, bench_v5);
criterion_main!(benches);
