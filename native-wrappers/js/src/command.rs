use bytes::Bytes;
use rumqttc_wrapper_core::{
    PublishCommand, PublishProtocolOptions, QoS, SubscribeCommand, SubscribeProtocolOptions,
    Subscription, SubscriptionProtocolOptions, UnsubscribeCommand, UnsubscribeProtocolOptions,
    V5OutgoingPublishProperties, V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions,
    V5UnsubscribeProperties,
};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishOptions {
    #[serde(default)]
    qos: u8,
    #[serde(default)]
    retain: bool,
    properties: Option<PublishProperties>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishProperties {
    response_topic: Option<String>,
    correlation_data: Option<Vec<u8>>,
    content_type: Option<String>,
    payload_format_indicator: Option<u8>,
    topic_alias: Option<u16>,
    message_expiry_interval: Option<u32>,
    #[serde(default)]
    user_properties: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubscriptionInput {
    filter: String,
    #[serde(default)]
    qos: u8,
    options: Option<SubscriptionOptions>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubscriptionOptions {
    #[serde(default)]
    no_local: bool,
    #[serde(default)]
    retain_as_published: bool,
    #[serde(default)]
    retain_forward_rule: u8,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubscribeOptions {
    subscription_identifier: Option<usize>,
    #[serde(default)]
    user_properties: Vec<(String, String)>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnsubscribeOptions {
    #[serde(default)]
    user_properties: Vec<(String, String)>,
}

pub fn publish(
    topic: String,
    payload: Vec<u8>,
    options: Option<&str>,
) -> Result<PublishCommand, String> {
    let options: PublishOptions = parse_or_default(options, "publish options")?;
    Ok(PublishCommand {
        topic,
        payload: Bytes::from(payload),
        qos: qos(options.qos)?,
        retain: options.retain,
        protocol: options
            .properties
            .map_or(PublishProtocolOptions::VersionNeutral, |p| {
                PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                    response_topic: p.response_topic,
                    correlation_data: p.correlation_data.map(Bytes::from),
                    content_type: p.content_type,
                    payload_format_indicator: p.payload_format_indicator,
                    topic_alias: p.topic_alias,
                    message_expiry_interval: p.message_expiry_interval,
                    user_properties: p.user_properties,
                })
            }),
    })
}

pub fn subscribe(filters: &str, options: Option<&str>) -> Result<SubscribeCommand, String> {
    let filters: Vec<SubscriptionInput> =
        serde_json::from_str(filters).map_err(|error| format!("invalid subscriptions: {error}"))?;
    let options: Option<SubscribeOptions> = parse_optional(options, "subscribe options")?;
    Ok(SubscribeCommand {
        filters: filters
            .into_iter()
            .map(|filter| {
                let protocol = match filter.options {
                    None => SubscriptionProtocolOptions::VersionNeutral,
                    Some(options) => SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                        no_local: options.no_local,
                        retain_as_published: options.retain_as_published,
                        retain_forward_rule: match options.retain_forward_rule {
                            0 => V5RetainForwardRule::OnEverySubscribe,
                            1 => V5RetainForwardRule::OnNewSubscribe,
                            2 => V5RetainForwardRule::Never,
                            _ => return Err("retainForwardRule must be 0, 1, or 2".to_owned()),
                        },
                    }),
                };
                Ok(Subscription {
                    filter: filter.filter,
                    qos: qos(filter.qos)?,
                    protocol,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        protocol: options.map_or(SubscribeProtocolOptions::VersionNeutral, |options| {
            SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: options.subscription_identifier,
                user_properties: options.user_properties,
            })
        }),
    })
}

pub fn unsubscribe(filters: &str, options: Option<&str>) -> Result<UnsubscribeCommand, String> {
    let filters: Vec<String> = serde_json::from_str(filters)
        .map_err(|error| format!("invalid unsubscribe filters: {error}"))?;
    let options: Option<UnsubscribeOptions> = parse_optional(options, "unsubscribe options")?;
    Ok(UnsubscribeCommand {
        filters,
        protocol: options.map_or(UnsubscribeProtocolOptions::VersionNeutral, |options| {
            UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties {
                user_properties: options.user_properties,
            })
        }),
    })
}

fn qos(value: u8) -> Result<QoS, String> {
    match value {
        0 => Ok(QoS::AtMostOnce),
        1 => Ok(QoS::AtLeastOnce),
        2 => Ok(QoS::ExactlyOnce),
        _ => Err("QoS must be 0, 1, or 2".to_owned()),
    }
}

fn parse_or_default<T>(value: Option<&str>, name: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    value.map_or_else(|| Ok(T::default()), |value| parse(value, name))
}

fn parse_optional<T>(value: Option<&str>, name: &str) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    value.map(|value| parse(value, name)).transpose()
}

fn parse<T>(value: &str, name: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(value).map_err(|error| format!("invalid {name}: {error}"))
}
