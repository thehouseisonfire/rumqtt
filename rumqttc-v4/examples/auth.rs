use bytes::Bytes;
use rumqttc::{ConnectAuth, MqttOptions};

fn main() {
    let mut options = MqttOptions::new("client-v4", "localhost");

    options.set_username("user-only");
    assert_eq!(
        options.auth(),
        &ConnectAuth::Username {
            username: "user-only".into(),
        }
    );

    options.set_credentials("user", Bytes::from_static(b"\x00\xfftoken"));
    assert_eq!(
        options.auth(),
        &ConnectAuth::UsernamePassword {
            username: "user".into(),
            password: Bytes::from_static(b"\x00\xfftoken"),
        }
    );

    options.clear_auth();
    assert_eq!(options.auth(), &ConnectAuth::None);
}
