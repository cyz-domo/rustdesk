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
            id: hbb_common::uuid::Uuid::new_v4().to_string(),
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
                let (host_str, _) = hbb_common::parse_as_ipv4_or_ipv6_or_domain(host);
                let my_api = api_opt.as_ref().filter(|a| is_host_match(&host_str, a)).cloned();
                let my_relay = relay_opt.as_ref().filter(|r| is_host_match(&host_str, r)).cloned();
                let my_key = if idx == 0 { key_opt.clone() } else { None };
                ServerProfile {
                    id: format!("profile-{}", idx + 1),
                    name: format!("Server {}", idx + 1),
                    host: host.to_string(),
                    relay: my_relay,
                    api: my_api,
                    key: my_key,
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

/// Helper to match server hosts with or without ports
pub fn is_host_match(h1: &str, h2: &str) -> bool {
    let (h1_clean, _) = hbb_common::parse_as_ipv4_or_ipv6_or_domain(h1);
    let (h2_clean, _) = hbb_common::parse_as_ipv4_or_ipv6_or_domain(h2);
    if !h1_clean.is_empty() && !h2_clean.is_empty() && h1_clean == h2_clean {
        return true;
    }
    if h1 == h2 || h1.starts_with(h2) || h2.starts_with(h1) {
        return true;
    }
    let host_only = |s: &str| s.split(':').next().unwrap_or(s).trim();
    host_only(h1) == host_only(h2)
}

/// Get a server profile by host.
pub fn get_profile_by_host(host: &str) -> Option<ServerProfile> {
    let profiles = get_server_profiles();
    for p in profiles {
        if is_host_match(&p.host, host) {
            return Some(p);
        }
        if let Some(ref tcp) = p.tcp_host {
            if is_host_match(tcp, host) {
                return Some(p);
            }
        }
        if let Some(ref relay) = p.relay {
            if is_host_match(relay, host) {
                return Some(p);
            }
        }
    }
    None
}

/// Get key associated with a specific host.
pub fn get_key_by_host(host: &str) -> String {
    if let Some(p) = get_profile_by_host(host) {
        if let Some(key) = p.key {
            return key;
        } else {
            // Profile explicitly has no key; do not leak another server's global key!
            return "".to_string();
        }
    }
    Config::get_option("key")
}

/// Get TCP host associated with a specific host.
pub fn get_tcp_host_by_host(host: &str) -> Option<String> {
    get_profile_by_host(host).and_then(|p| p.tcp_host)
}

/// Get relay associated with a specific host.
pub fn get_relay_by_host(host: &str) -> Option<String> {
    get_profile_by_host(host).and_then(|p| p.relay)
}

/// Get API associated with a specific host.
pub fn get_api_by_host(host: &str) -> Option<String> {
    get_profile_by_host(host).and_then(|p| p.api)
}

lazy_static::lazy_static! {
    static ref SERVER_LATENCIES: std::sync::Mutex<std::collections::HashMap<String, i64>> = Default::default();
}

pub fn update_server_profile_latency(id: &str, configured_host: &str, resolved_host: &str, latency: i64) {
    if let Ok(mut map) = SERVER_LATENCIES.lock() {
        if !id.is_empty() {
            map.insert(id.to_string(), latency);
        }
        if !configured_host.is_empty() {
            map.insert(configured_host.to_string(), latency);
        }
        if !resolved_host.is_empty() {
            map.insert(resolved_host.to_string(), latency);
        }
        if let Ok(json) = serde_json::to_string(&*map) {
            Config::set_option("server-latencies".to_owned(), json);
        }
    }
    Config::update_latency(resolved_host, latency);
}

pub fn update_server_latency(host: &str, latency: i64) {
    update_server_profile_latency("", host, host, latency);
}

pub fn get_server_latency_by_profile(id: &str, host: &str) -> i64 {
    if let Ok(map) = SERVER_LATENCIES.lock() {
        if !id.is_empty() {
            if let Some(&lat) = map.get(id) {
                return lat;
            }
        }
        if let Some(&lat) = map.get(host) {
            return lat;
        }
        for (k, v) in map.iter() {
            if is_host_match(k, host) {
                return *v;
            }
        }
    }
    // Fallback: check synced "server-latencies" option (syncs over IPC to UI process)
    let raw = Config::get_option("server-latencies");
    if !raw.is_empty() {
        if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, i64>>(&raw) {
            if !id.is_empty() {
                if let Some(&lat) = map.get(id) {
                    return lat;
                }
            }
            if let Some(&lat) = map.get(host) {
                return lat;
            }
            for (k, v) in map.iter() {
                if is_host_match(k, host) {
                    return *v;
                }
            }
        }
    }
    -1
}

pub fn get_server_latency(host: &str) -> i64 {
    get_server_latency_by_profile("", host)
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
            let lat_us = get_server_latency_by_profile(&p.id, &p.host);
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

