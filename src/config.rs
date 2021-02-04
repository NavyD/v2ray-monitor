use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Log {
    loglevel: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Inbound {
    sniffing: Option<InboundSniffing>,
    port: Option<u16>,
    protocol: Option<String>,
    settings: Option<InboundSettings>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InboundSniffing {
    enabled: Option<bool>,
    dest_override: Option<Vec<String>>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InboundSettings {
    udp: Option<bool>,
    auth: Option<String>,
    ip: Option<String>,
    network: Option<String>,
    follow_redirect: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Outbound {
    settings: Option<InboundSettings>,
    protocol: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutboundSettings {
    vnext: Option<Vec<OutboundVnextItem>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutboundVnextItem {
    port: Option<u16>,
    users: Option<Vec<OutboundVnextItemUsersItem>>,
    address: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutboundVnextItemUsersItem {
    id: Option<String>,
    alter_id: Option<u32>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OutboundStreamSettings {
    network: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]

pub struct OutboundStreamSettingsWsSettings {}
