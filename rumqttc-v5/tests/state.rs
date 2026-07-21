use rumqttc::{MqttState, Packet, Publish, QoS, Request};

fn replayed_publish_from_clean(requests: Vec<Request>) -> Publish {
    requests
        .into_iter()
        .find_map(|request| match request {
            Request::Publish(publish) => Some(publish),
            _ => None,
        })
        .expect("expected pending publish replay")
}

fn queue_outgoing_publish(state: &mut MqttState, qos: QoS) -> Packet {
    state
        .handle_outgoing_packet(Request::Publish(Publish::new(
            "hello/world",
            qos,
            vec![1, 2, 3],
            None,
        )))
        .unwrap()
        .expect("expected outgoing publish")
}

#[test]
fn direct_state_clean_marks_qos1_publish_for_dup_replay() {
    let mut state = MqttState::builder(10).build();
    let packet = queue_outgoing_publish(&mut state, QoS::AtLeastOnce);

    assert!(matches!(packet, Packet::Publish(_)));

    let replay = replayed_publish_from_clean(state.clean());

    assert!(replay.dup);
    assert_eq!(replay.qos, QoS::AtLeastOnce);
}

#[test]
fn direct_state_clean_marks_qos2_publish_for_dup_replay() {
    let mut state = MqttState::builder(10).build();
    let packet = queue_outgoing_publish(&mut state, QoS::ExactlyOnce);

    assert!(matches!(packet, Packet::Publish(_)));

    let replay = replayed_publish_from_clean(state.clean());

    assert!(replay.dup);
    assert_eq!(replay.qos, QoS::ExactlyOnce);
}

#[test]
fn direct_state_clean_marks_unflushed_qos1_publish_for_conservative_replay() {
    let mut state = MqttState::builder(10).build();
    queue_outgoing_publish(&mut state, QoS::AtLeastOnce);

    let replay = replayed_publish_from_clean(state.clean());

    assert!(replay.dup);
    assert_eq!(replay.qos, QoS::AtLeastOnce);
}
