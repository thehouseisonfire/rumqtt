use bytes::Bytes;
use flume::bounded;
use rumqttc::mqttbytes::{QoS, v5::AuthProperties};
use rumqttc::{AuthManager, Client, MqttOptions};
#[cfg(feature = "auth-scram")]
use scram::{
    ChannelBindType, ScramAuthClient, ScramCbHelper, ScramNonce, ScramResultClient,
    ScramSha256RustNative, scram_sync::SyncScramClient,
};
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::thread;

#[cfg(feature = "auth-scram")]
type ScramSha256Client<'a> =
    SyncScramClient<ScramSha256RustNative, ScramCredentials<'a>, ScramCredentials<'a>>;

#[cfg(feature = "auth-scram")]
#[derive(Clone, Copy, Debug)]
struct ScramCredentials<'a> {
    user: &'a str,
    password: &'a str,
}

#[cfg(feature = "auth-scram")]
impl ScramAuthClient for ScramCredentials<'_> {
    fn get_username(&self) -> &str {
        self.user
    }

    fn get_password(&self) -> &str {
        self.password
    }
}

#[cfg(feature = "auth-scram")]
impl ScramCbHelper for ScramCredentials<'_> {}

#[derive(Debug)]
struct ScramAuthManager<'a> {
    #[allow(dead_code)]
    user: &'a str,
    #[allow(dead_code)]
    password: &'a str,
    #[cfg(feature = "auth-scram")]
    scram: Option<ScramSha256Client<'a>>,
}

impl<'a> ScramAuthManager<'a> {
    fn new(user: &'a str, password: &'a str) -> ScramAuthManager<'a> {
        ScramAuthManager {
            user,
            password,
            #[cfg(feature = "auth-scram")]
            scram: None,
        }
    }

    #[cfg(feature = "auth-scram")]
    fn new_scram_client(&self) -> Result<ScramSha256Client<'a>, String> {
        let credentials = ScramCredentials {
            user: self.user,
            password: self.password,
        };

        SyncScramClient::new(
            credentials,
            ScramNonce::none().map_err(|e| e.to_string())?,
            ChannelBindType::None,
            credentials,
            false,
        )
        .map_err(|e| e.to_string())
    }

    fn auth_start(&mut self) -> Result<Option<Bytes>, String> {
        #[cfg(feature = "auth-scram")]
        {
            let mut scram = self.new_scram_client()?;
            let client_first = scram
                .init_client()
                .unwrap_output()
                .map_err(|e| e.to_string())?;
            self.scram = Some(scram);

            Ok(Some(client_first.into()))
        }

        #[cfg(not(feature = "auth-scram"))]
        Ok(Some("client first message".into()))
    }
}

impl<'a> AuthManager for ScramAuthManager<'a> {
    fn auth_continue(
        &mut self,
        #[allow(unused_variables)] auth_prop: Option<AuthProperties>,
    ) -> Result<Option<AuthProperties>, String> {
        #[cfg(feature = "auth-scram")]
        {
            let prop = auth_prop.ok_or_else(|| "Missing authentication properties".to_string())?;

            if prop.method.as_deref() != Some("SCRAM-SHA-256") {
                return Err("Invalid authentication method".to_string());
            }

            let scram = self
                .scram
                .as_mut()
                .ok_or_else(|| "Invalid state".to_string())?;

            let auth_data = prop
                .data
                .ok_or_else(|| "Missing authentication data".to_string())
                .and_then(|data| String::from_utf8(data.to_vec()).map_err(|e| e.to_string()))?;

            let client_final = match scram.parse_response(&auth_data) {
                Ok(ScramResultClient::Output(client_final)) => client_final,
                Ok(ScramResultClient::Completed) => {
                    self.scram = None;
                    return Ok(None);
                }
                Err(e) => return Err(e.to_string()),
            };

            Ok(Some(AuthProperties {
                method: Some("SCRAM-SHA-256".to_string()),
                data: Some(client_final.into()),
                reason: None,
                user_properties: Vec::new(),
            }))
        }

        #[cfg(not(feature = "auth-scram"))]
        Ok(Some(AuthProperties {
            method: Some("SCRAM-SHA-256".to_string()),
            data: Some("client final message".into()),
            reason: None,
            user_properties: Vec::new(),
        }))
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut authmanager = ScramAuthManager::new("user1", "123456");
    let client_first = authmanager.auth_start().unwrap();
    let authmanager = Arc::new(Mutex::new(authmanager));

    let mut mqttoptions = MqttOptions::new("auth_test", "127.0.0.1");
    mqttoptions.set_authentication_method(Some("SCRAM-SHA-256".to_string()));
    mqttoptions.set_authentication_data(client_first);
    mqttoptions.set_auth_manager(authmanager.clone());
    let (client, mut connection) = Client::new(mqttoptions, 10);

    let (tx, rx) = bounded(1);

    thread::spawn(move || {
        client
            .subscribe("rumqtt_auth/topic", QoS::AtLeastOnce)
            .unwrap();
        client
            .publish("rumqtt_auth/topic", QoS::AtLeastOnce, false, "hello world")
            .unwrap();

        // Wait for the connection to be established.
        rx.recv().unwrap();

        // Reauthenticate using SCRAM-SHA-256
        let client_first = authmanager.clone().lock().unwrap().auth_start().unwrap();
        let properties = AuthProperties {
            method: Some("SCRAM-SHA-256".to_string()),
            data: client_first,
            reason: None,
            user_properties: Vec::new(),
        };
        client.reauth(Some(properties)).unwrap();
    });

    for notification in connection.iter() {
        match notification {
            Ok(event) => {
                println!("Event = {:?}", event);
                if let rumqttc::Event::Incoming(rumqttc::Incoming::ConnAck(_)) = event {
                    tx.send("Connected").unwrap();
                }
            }
            Err(e) => {
                println!("Error = {:?}", e);
                break;
            }
        }
    }

    Ok(())
}
