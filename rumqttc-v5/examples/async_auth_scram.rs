use bytes::Bytes;
use flume::bounded;
use rumqttc::PublishOptions;
use rumqttc::mqttbytes::{QoS, v5::AuthProperties};
use rumqttc::{AsyncClient, AuthAction, AuthContext, AuthError, Authenticator, MqttOptions};
#[cfg(feature = "auth-scram")]
use scram::{
    ChannelBindType, ScramAuthClient, ScramCbHelper, ScramNonce, ScramResultClient,
    ScramSha256RustNative, scram_sync::SyncScramClient,
};
use std::error::Error;
use std::sync::{Arc, Mutex};
use tokio::task;

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
    user: &'a str,
    password: &'a str,
    #[cfg(feature = "auth-scram")]
    scram: Option<ScramSha256Client<'a>>,
}

impl<'a> ScramAuthManager<'a> {
    const fn new(user: &'a str, password: &'a str) -> Self {
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

impl Authenticator for ScramAuthManager<'_> {
    fn start(&mut self, _context: AuthContext<'_>) -> Result<Option<AuthProperties>, AuthError> {
        self.auth_start()
            .map(|data| {
                data.map(|data| AuthProperties {
                    method: Some("SCRAM-SHA-256".to_string()),
                    data: Some(data),
                    reason: None,
                    user_properties: Vec::new(),
                })
            })
            .map_err(AuthError::from)
    }

    fn continue_auth(
        &mut self,
        _context: AuthContext<'_>,
        #[allow(unused_variables)] auth_prop: Option<AuthProperties>,
    ) -> Result<AuthAction, AuthError> {
        #[cfg(feature = "auth-scram")]
        {
            let prop =
                auth_prop.ok_or_else(|| AuthError::from("Missing authentication properties"))?;

            if prop.method.as_deref() != Some("SCRAM-SHA-256") {
                return Err(AuthError::from("Invalid authentication method"));
            }

            let scram = self
                .scram
                .as_mut()
                .ok_or_else(|| AuthError::from("Invalid state"))?;

            let auth_data = prop
                .data
                .ok_or_else(|| AuthError::from("Missing authentication data"))
                .and_then(|data| {
                    String::from_utf8(data.to_vec()).map_err(|e| AuthError::from(e.to_string()))
                })?;

            let client_final = match scram.parse_response(&auth_data) {
                Ok(ScramResultClient::Output(client_final)) => client_final,
                Ok(ScramResultClient::Completed) => {
                    self.scram = None;
                    return Ok(AuthAction::Complete);
                }
                Err(e) => return Err(AuthError::from(e.to_string())),
            };

            Ok(AuthAction::Send(AuthProperties {
                method: Some("SCRAM-SHA-256".to_string()),
                data: Some(client_final.into()),
                reason: None,
                user_properties: Vec::new(),
            }))
        }

        #[cfg(not(feature = "auth-scram"))]
        Ok(AuthAction::Send(AuthProperties {
            method: Some("SCRAM-SHA-256".to_string()),
            data: Some("client final message".into()),
            reason: None,
            user_properties: Vec::new(),
        }))
    }

    fn success(
        &mut self,
        _context: AuthContext<'_>,
        _incoming: Option<AuthProperties>,
    ) -> Result<(), AuthError> {
        #[cfg(feature = "auth-scram")]
        {
            self.scram = None;
        }
        Ok(())
    }

    fn failure(&mut self, _context: AuthContext<'_>, _error: AuthError) {
        #[cfg(feature = "auth-scram")]
        {
            self.scram = None;
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let authmanager = ScramAuthManager::new("user1", "123456");
    let authmanager = Arc::new(Mutex::new(authmanager));

    let mut mqttoptions = MqttOptions::new("auth_test", "127.0.0.1");
    mqttoptions.set_authentication_method(Some("SCRAM-SHA-256".to_string()));
    mqttoptions.set_auth_manager(authmanager.clone());
    let (client, mut eventloop) = AsyncClient::builder(mqttoptions).capacity(10).build();

    let (tx, rx) = bounded(1);

    task::spawn(async move {
        client
            .subscribe("rumqtt_auth/topic", QoS::AtLeastOnce)
            .await
            .unwrap();
        client
            .publish(
                "rumqtt_auth/topic",
                "hello world",
                PublishOptions::new(QoS::AtLeastOnce),
            )
            .await
            .unwrap();

        // Wait for the connection to be established.
        rx.recv_async().await.unwrap();

        // Reauthenticate using SCRAM-SHA-256
        client.reauth(None).await.unwrap();
    });

    loop {
        let notification = eventloop.poll().await;

        match notification {
            Ok(event) => {
                println!("Event = {event:?}");
                if let rumqttc::Event::Incoming(rumqttc::Incoming::ConnAck(_)) = event {
                    tx.send_async("Connected").await.unwrap();
                }
            }
            Err(e) => {
                println!("Error = {e:?}");
                break;
            }
        }
    }

    Ok(())
}
