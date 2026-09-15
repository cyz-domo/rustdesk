use serde::{Deserialize, Serialize};
use hbb_common::config::Config;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerProfile {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub tcp_host: Option<String>,
    #[serde(default)]
    pub relay: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub online: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl ServerProfile {
    pub fn new(name: String, host: String) -> Self {
        Self {
            id: hbb_common::get_uuid(),
            name,
            host,
            enabled: true,
            ..Default::default()
        }
    }
}

/// Retrieve the configured server profiles list.
/// If no profile list exists yet, it dynamically parses existing `custom-rendezvous-server`
/// (which may contain multiple servers separated by `;`, `,` or newlines).
pub fn get_server_profiles() -> Vec<ServerProfile> {
    let raw = Config::get_option("server-profiles");
    if !raw.is_empty() {
        if let Ok(list) = serde_json::from_str::<Vec<ServerProfile>>(&raw) {
            if !list.is_empty() {
                return list;
            }
        }
    }

    // Fallback: parse from custom-rendezvous-server
    let custom = Config::get_option("custom-rendezvous-server");
    let global_relay = Config::get_option("relay-server");
    let global_api = Config::get_option("api-server");
    let global_key = Config::get_option("key");

    let relay_opt = if !global_relay.is_empty() { Some(global_relay) } else { None };
    let api_opt = if !global_api.is_empty() { Some(global_api) } else { None };
    let key_opt = if !global_key.is_empty() { Some(global_key) } else { None };

    if !custom.is_empty() {
        let parts: Vec<&str> = custom.split(&[';', ',', '\n'][..]).map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if parts.len() > 1 {
            return parts.into_iter().enumerate().map(|(idx, host)| {
                ServerProfile {
                    id: format!("profile-{}", idx + 1),
                    name: format!("Server {}", idx + 1),
                    host: host.to_string(),
                    relay: relay_opt.clone(),
                    api: api_opt.clone(),
                    key: key_opt.clone(),
                    enabled: true,
                    ..Default::default()
                }
            }).collect();
        } else if let Some(&host) = parts.first() {
            return vec![ServerProfile {
                id: "profile-1".to_string(),
                name: "默认服务器".to_string(),
                host: host.to_string(),
                relay: relay_opt,
                api: api_opt,
                key: key_opt,
                enabled: true,
                ..Default::default()
            }];
        }
    }

    // Check rendezvous-servers or public servers
    let servers = Config::get_rendezvous_servers();
    servers.into_iter().enumerate().map(|(idx, host)| {
        ServerProfile {
            id: format!("default-{}", idx + 1),
            name: if idx == 0 { "公共主服务器".to_string() } else { format!("公共服务器 {}", idx + 1) },
            host,
            enabled: true,
            ..Default::default()
        }
    }).collect()
}

/// Save the server profiles list to options.
pub fn set_server_profiles(profiles: &[ServerProfile]) {
    if let Ok(json) = serde_json::to_string(profiles) {
        Config::set_option("server-profiles".to_owned(), json);
    }
}

/// Get all currently enabled server profiles.
pub fn get_active_server_profiles() -> Vec<ServerProfile> {
    let all = get_server_profiles();
    let active: Vec<ServerProfile> = all.into_iter().filter(|p| p.enabled).collect();
    if active.is_empty() {
        get_server_profiles()
    } else {
        active
    }
}

/// Get key associated with a specific host.
pub fn get_key_by_host(host: &str) -> String {
    for p in get_server_profiles() {
        if p.host == host || host.starts_with(&p.host) || p.host.starts_with(host) {
            if let Some(key) = &p.key {
                if !key.is_empty() {
                    return key.clone();
                }
            }
        }
    }
    Config::get_option("key")
}

lazy_static::lazy_static! {
    static ref SERVER_LATENCIES: std::sync::Mutex<std::collections::HashMap<String, i64>> = Default::default();
}

pub fn update_server_latency(host: &str, latency: i64) {
    if let Ok(mut map) = SERVER_LATENCIES.lock() {
        map.insert(host.to_string(), latency);
    }
    Config::update_latency(host, latency);
}

pub fn get_server_latency(host: &str) -> i64 {
    if let Ok(map) = SERVER_LATENCIES.lock() {
        if let Some(&lat) = map.get(host) {
            return lat;
        }
        for (k, v) in map.iter() {
            if k == host || host.starts_with(k) || k.starts_with(host) {
                return *v;
            }
        }
    }
    -1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerProfileStatus {
    pub id: String,
    pub name: String,
    pub host: String,
    pub enabled: bool,
    pub online: bool,
    pub latency_ms: i64,
}

pub fn get_server_profile_statuses() -> Vec<ServerProfileStatus> {
    let profiles = get_server_profiles();
    profiles
        .into_iter()
        .map(|p| {
            let lat_us = get_server_latency(&p.host);
            let online = p.enabled && lat_us > 0;
            let latency_ms = if lat_us > 0 { (lat_us + 999) / 1000 } else { -1 };
            ServerProfileStatus {
                id: p.id,
                name: p.name,
                host: p.host,
                enabled: p.enabled,
                online,
                latency_ms,
            }
        })
        .collect()
}

