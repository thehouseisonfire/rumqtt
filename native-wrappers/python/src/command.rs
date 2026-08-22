use bytes::Bytes;
use rumqttc_wrapper_core::{
    PublishCommand, PublishProtocolOptions, QoS, SubscribeCommand, SubscribeProtocolOptions,
    Subscription, SubscriptionProtocolOptions, UnsubscribeCommand, UnsubscribeProtocolOptions,
    V5OutgoingPublishProperties, V5RetainForwardRule, V5SubscribeProperties, V5SubscriptionOptions,
    V5UnsubscribeProperties,
};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Publish {
    #[serde(default)]
    qos: u8,
    #[serde(default)]
    retain: bool,
    properties: Option<PublishProperties>,
}
#[derive(Default, Deserialize)]
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
struct Sub {
    filter: String,
    #[serde(default)]
    qos: u8,
    options: Option<SubOptions>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubOptions {
    #[serde(default)]
    no_local: bool,
    #[serde(default)]
    retain_as_published: bool,
    #[serde(default)]
    retain_forward_rule: u8,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubscribeOptions {
    subscription_identifier: Option<usize>,
    #[serde(default)]
    user_properties: Vec<(String, String)>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnsubscribeOptions {
    #[serde(default)]
    user_properties: Vec<(String, String)>,
}

pub fn publish(
    topic: String,
    payload: Vec<u8>,
    value: Option<&str>,
) -> Result<PublishCommand, String> {
    let p: Publish =
        value.map_or_else(|| Ok(Publish::default()), |v| parse(v, "publish options"))?;
    Ok(PublishCommand {
        topic,
        payload: Bytes::from(payload),
        qos: qos(p.qos)?,
        retain: p.retain,
        protocol: p
            .properties
            .map_or(PublishProtocolOptions::VersionNeutral, |v| {
                PublishProtocolOptions::V5(V5OutgoingPublishProperties {
                    response_topic: v.response_topic,
                    correlation_data: v.correlation_data.map(Bytes::from),
                    content_type: v.content_type,
                    payload_format_indicator: v.payload_format_indicator,
                    topic_alias: v.topic_alias,
                    message_expiry_interval: v.message_expiry_interval,
                    user_properties: v.user_properties,
                })
            }),
    })
}
pub fn subscribe(filters: &str, value: Option<&str>) -> Result<SubscribeCommand, String> {
    let filters: Vec<Sub> = parse(filters, "subscriptions")?;
    let options: Option<SubscribeOptions> =
        value.map(|v| parse(v, "subscribe options")).transpose()?;
    let filters = filters
        .into_iter()
        .map(|v| {
            let protocol = match v.options {
                None => SubscriptionProtocolOptions::VersionNeutral,
                Some(o) => SubscriptionProtocolOptions::V5(V5SubscriptionOptions {
                    no_local: o.no_local,
                    retain_as_published: o.retain_as_published,
                    retain_forward_rule: match o.retain_forward_rule {
                        0 => V5RetainForwardRule::OnEverySubscribe,
                        1 => V5RetainForwardRule::OnNewSubscribe,
                        2 => V5RetainForwardRule::Never,
                        _ => return Err("retainForwardRule must be 0, 1, or 2".into()),
                    },
                }),
            };
            Ok(Subscription {
                filter: v.filter,
                qos: qos(v.qos)?,
                protocol,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(SubscribeCommand {
        filters,
        protocol: options.map_or(SubscribeProtocolOptions::VersionNeutral, |o| {
            SubscribeProtocolOptions::V5(V5SubscribeProperties {
                subscription_identifier: o.subscription_identifier,
                user_properties: o.user_properties,
            })
        }),
    })
}
pub fn unsubscribe(filters: &str, value: Option<&str>) -> Result<UnsubscribeCommand, String> {
    let filters = parse(filters, "unsubscribe filters")?;
    let options: Option<UnsubscribeOptions> =
        value.map(|v| parse(v, "unsubscribe options")).transpose()?;
    Ok(UnsubscribeCommand {
        filters,
        protocol: options.map_or(UnsubscribeProtocolOptions::VersionNeutral, |o| {
            UnsubscribeProtocolOptions::V5(V5UnsubscribeProperties {
                user_properties: o.user_properties,
            })
        }),
    })
}
fn qos(v: u8) -> Result<QoS, String> {
    match v {
        0 => Ok(QoS::AtMostOnce),
        1 => Ok(QoS::AtLeastOnce),
        2 => Ok(QoS::ExactlyOnce),
        _ => Err("QoS must be 0, 1, or 2".into()),
    }
}
fn parse<T: for<'a> Deserialize<'a>>(v: &str, name: &str) -> Result<T, String> {
    serde_json::from_str(v).map_err(|e| format!("invalid {name}: {e}"))
}
