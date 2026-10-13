use rumqttc::{ClientBuildError, ConfigError};

// This exhaustive match is intentionally compiled outside the library, with and without
// bounded-publish. Enabling a dependency feature must not break existing callers.
fn configuration_error(error: &ClientBuildError) -> Option<&ConfigError> {
    match error {
        ClientBuildError::Config(error) => Some(error),
        ClientBuildError::Runtime(_) => None,
    }
}

#[test]
fn client_build_error_preserves_its_original_variants() {
    let error = ClientBuildError::Config(ConfigError::ReceiveMaximum);
    assert_eq!(
        configuration_error(&error),
        Some(&ConfigError::ReceiveMaximum)
    );
    let error = ClientBuildError::Runtime(std::io::Error::other("runtime creation failed"));
    assert_eq!(configuration_error(&error), None);
}

#[cfg(feature = "bounded-publish")]
#[test]
fn invalid_publish_limits_are_configuration_errors_for_both_builders() {
    use rumqttc::{AsyncClient, Client, MqttOptions, PublishBudgetLimits};

    for (max_outstanding, max_bytes) in [(0, 1), (1, 0), (0, 0)] {
        let options = MqttOptions::new("invalid-budget", "localhost");
        let limits = PublishBudgetLimits {
            max_outstanding,
            max_bytes,
        };
        let error = AsyncClient::builder(options.clone())
            .publish_budget(limits)
            .try_build()
            .err()
            .expect("zero publish budget limit must be rejected");
        assert_eq!(
            configuration_error(&error),
            Some(&ConfigError::InvalidPublishBudget)
        );
        let error = Client::builder(options)
            .publish_budget(limits)
            .try_build()
            .err()
            .expect("zero publish budget limit must be rejected");
        assert_eq!(
            configuration_error(&error),
            Some(&ConfigError::InvalidPublishBudget)
        );
    }
}

#[test]
fn default_builders_preserve_offline_queueing_without_managed_admission() {
    use rumqttc::{AsyncClient, MqttOptions, PublishOptions};

    let (client, _driver) = AsyncClient::builder(MqttOptions::new("legacy", "localhost"))
        .capacity(1)
        .build();
    assert!(client.publish_admission_waiter().is_none());
    #[cfg(feature = "bounded-publish")]
    assert_eq!(client.publish_budget_snapshot(), None);
    // QoS 1 remains admissible before CONNACK under the default policy.
    client
        .try_publish("a", "data", PublishOptions::at_least_once())
        .unwrap();
}
