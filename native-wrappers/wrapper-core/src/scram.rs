use std::time::Duration;

use crate::{Error, SecretBytes};

/// Built-in SCRAM-SHA-256 without channel binding. Use authenticated TLS to
/// protect the exchange. Every client gets independent exchange state, including
/// when configurations are cloned. Server signatures are verified before success.
#[derive(Clone, PartialEq, Eq)]
pub struct ScramConfig {
    pub username: String,
    pub password: SecretBytes,
    pub exchange_timeout: Duration,
    /// Bound server-selected PBKDF2 work before computing a response. Accepted
    /// iteration counts are 4096 through this value (at most 100,000).
    pub max_iterations: u32,
}

impl ScramConfig {
    pub fn new(username: impl Into<String>, password: SecretBytes) -> Self {
        Self {
            username: username.into(),
            password,
            exchange_timeout: Duration::from_secs(30),
            max_iterations: 100_000,
        }
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        if !cfg!(feature = "auth-scram") {
            return Err(Error::configuration("SCRAM feature is disabled"));
        }
        crate::config::validate_mqtt_utf8_string(&self.username, "SCRAM username")?;
        if self.username.is_empty() || std::str::from_utf8(self.password.expose()).is_err() {
            return Err(Error::configuration(
                "SCRAM requires a nonempty username and UTF-8 password",
            ));
        }
        if !(4096..=100_000).contains(&self.max_iterations) {
            return Err(Error::configuration(
                "SCRAM maximum iterations must be between 4096 and 100000",
            ));
        }
        if self.exchange_timeout.is_zero()
            || std::time::Instant::now()
                .checked_add(self.exchange_timeout)
                .is_none()
        {
            return Err(Error::configuration("invalid SCRAM exchange timeout"));
        }
        Ok(())
    }
}

impl std::fmt::Debug for ScramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScramConfig")
            .field("credentials", &"[REDACTED]")
            .field("exchange_timeout", &self.exchange_timeout)
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

#[cfg(feature = "auth-scram")]
pub fn build(config: ScramConfig) -> crate::AuthenticatorConfig {
    let timeout = config.exchange_timeout;
    crate::AuthenticatorConfig {
        authenticator: std::sync::Arc::new(implementation::Mechanism {
            config,
            state: parking_lot::Mutex::new(implementation::State::default()),
        }),
        exchange_timeout: timeout,
    }
}

#[cfg(feature = "auth-scram")]
mod implementation {
    use super::ScramConfig;
    use crate::{
        AuthAction, AuthChallenge, AuthContext, AuthFailure, AuthProperties, Authenticator,
    };
    use scram::{
        ChannelBindType, ScramAuthClient, ScramCbHelper, ScramNonce, ScramResultClient,
        ScramSha256RustNative, scram_sync::SyncScramClient,
    };

    #[derive(Clone, Debug)]
    struct Credentials(ScramConfig);
    impl ScramAuthClient for Credentials {
        fn get_username(&self) -> &str {
            &self.0.username
        }
        fn get_password(&self) -> &str {
            std::str::from_utf8(self.0.password.expose()).expect("validated UTF-8 password")
        }
    }
    impl ScramCbHelper for Credentials {}
    type Client = SyncScramClient<ScramSha256RustNative, Credentials, Credentials>;

    #[derive(Default)]
    pub(super) struct State {
        client: Option<Client>,
        verified: bool,
        awaiting_first: bool,
    }
    pub(super) struct Mechanism {
        pub config: ScramConfig,
        pub state: parking_lot::Mutex<State>,
    }

    impl Mechanism {
        fn receive(
            &self,
            state: &mut State,
            incoming: Option<AuthProperties>,
        ) -> Result<AuthAction, AuthFailure> {
            let incoming = incoming.ok_or(AuthFailure::InvalidResponse)?;
            if incoming
                .method
                .as_deref()
                .is_some_and(|method| method != "SCRAM-SHA-256")
            {
                return Err(AuthFailure::Method);
            }
            let data = incoming.data.ok_or(AuthFailure::InvalidResponse)?;
            let data = std::str::from_utf8(&data).map_err(|_| AuthFailure::InvalidResponse)?;
            if state.awaiting_first {
                let iterations = data
                    .split(',')
                    .filter_map(|part| part.strip_prefix("i="))
                    .collect::<Vec<_>>();
                if iterations.len() != 1
                    || !iterations[0]
                        .parse::<u32>()
                        .is_ok_and(|n| (4096..=self.config.max_iterations).contains(&n))
                {
                    return Err(AuthFailure::Rejected);
                }
            }
            let result = state
                .client
                .as_mut()
                .ok_or(AuthFailure::InvalidResponse)?
                .parse_response(data)
                .map_err(|_| AuthFailure::Rejected)?;
            state.awaiting_first = false;
            match result {
                ScramResultClient::Output(data) => Ok(send(data)),
                ScramResultClient::Completed => {
                    state.client = None;
                    state.verified = true;
                    Ok(AuthAction::Complete)
                }
            }
        }
    }

    fn send(data: String) -> AuthAction {
        AuthAction::Send(AuthProperties {
            method: Some("SCRAM-SHA-256".into()),
            data: Some(data.into()),
            ..Default::default()
        })
    }

    impl Authenticator for Mechanism {
        fn respond(
            &self,
            context: AuthContext,
            challenge: AuthChallenge,
        ) -> Result<AuthAction, AuthFailure> {
            if context.method != "SCRAM-SHA-256" {
                return Err(AuthFailure::Method);
            }
            let mut state = self.state.lock();
            let result = (|| match challenge {
                AuthChallenge::Start => {
                    *state = State::default();
                    let credentials = Credentials(self.config.clone());
                    let mut client = SyncScramClient::new(
                        credentials.clone(),
                        ScramNonce::none().map_err(|_| AuthFailure::Rejected)?,
                        ChannelBindType::None,
                        credentials,
                        false,
                    )
                    .map_err(|_| AuthFailure::Rejected)?;
                    let first = client
                        .init_client()
                        .unwrap_output()
                        .map_err(|_| AuthFailure::Rejected)?;
                    state.client = Some(client);
                    state.awaiting_first = true;
                    Ok(send(first))
                }
                AuthChallenge::Continue(incoming) => self.receive(&mut state, incoming),
                AuthChallenge::Success(incoming) => {
                    if state.verified
                        && incoming
                            .as_ref()
                            .is_some_and(|properties| properties.data.is_some())
                    {
                        return Err(AuthFailure::InvalidResponse);
                    }
                    if !state.verified {
                        self.receive(&mut state, incoming)?;
                    }
                    if !state.verified {
                        return Err(AuthFailure::Rejected);
                    }
                    *state = State::default();
                    Ok(AuthAction::Complete)
                }
                AuthChallenge::Failed => {
                    *state = State::default();
                    Ok(AuthAction::Complete)
                }
            })();
            if result.is_err() {
                *state = State::default();
            }
            result
        }
    }
}
