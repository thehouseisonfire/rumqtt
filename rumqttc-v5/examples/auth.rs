use bytes::Bytes;
use rumqttc::{ConnectAuth, MqttOptions};

fn main() {
    let mut options = MqttOptions::new("client-v5", "localhost");

    options.set_password(Bytes::from_static(b"\x00\xfftoken"));
    assert_eq!(
        options.auth(),
        &ConnectAuth::Password {
            password: Bytes::from_static(b"\x00\xfftoken"),
        }
    );

    options.set_auth(ConnectAuth::UsernamePassword {
        username: "user".into(),
        password: Bytes::from_static(b"pw"),
    });
    assert_eq!(
        options.auth(),
        &ConnectAuth::UsernamePassword {
            username: "user".into(),
            password: Bytes::from_static(b"pw"),
        }
    );

    options.clear_auth();
    assert_eq!(options.auth(), &ConnectAuth::None);
}
