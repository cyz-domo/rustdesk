use serde::{Deserialize, Serialize};
use hbb_common::config::{Config, LocalConfig, Status};

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
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub user_info: Option<String>,
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
            let filtered: Vec<ServerProfile> = list
                .into_iter()
                .filter(|p| !p.host.trim().is_empty())
                .collect();
            if !filtered.is_empty() {
                return filtered;
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
                let host_str = parse_host(host);
                let my_api = api_opt.as_ref().filter(|a| is_host_match(host_str, a)).cloned();
                let my_relay = relay_opt.as_ref().filter(|r| is_host_match(host_str, r)).cloned();
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
    let mut servers = Config::get_rendezvous_servers();
    let cur = Config::get_rendezvous_server();
    if !cur.is_empty() && !servers.iter().any(|s| is_same_rendezvous_host(s, &cur)) {
        servers.insert(0, cur);
    }
    servers
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .enumerate()
        .map(|(idx, host)| {
            ServerProfile {
                id: format!("default-{}", idx + 1),
                name: if idx == 0 { "官方服务器".to_string() } else { format!("官方服务器 {}", idx + 1) },
                host,
                enabled: true,
                ..Default::default()
            }
        })
        .collect()
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
    all.into_iter().filter(|p| p.enabled && !p.host.trim().is_empty()).collect()
}

/// Helper to extract host from host:port or [ipv6]:port
pub fn parse_host(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('[') {
        if let Some(end) = s.find(']') {
            return &s[1..end];
        }
    }
    if let Some(colon) = s.rfind(':') {
        if s.matches(':').count() == 1 {
            return &s[..colon];
        }
    }
    s
}

/// Helper to check if a host is an official RustDesk server
pub fn is_official_server(host: &str) -> bool {
    let h = parse_host(host);
    if h.is_empty() {
        return false;
    }
    if h.eq_ignore_ascii_case("public")
        || h.eq_ignore_ascii_case("rustdesk.com")
        || h.to_ascii_lowercase().ends_with(".rustdesk.com")
    {
        return true;
    }
    for &s in hbb_common::config::RENDEZVOUS_SERVERS {
        let sh = parse_host(s);
        if !sh.is_empty() && is_host_match(sh, h) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedProfileData {
    pub profile_id: String,
    pub original_host: String,
    pub resolved_host: String,
    pub tcp_host: Option<String>,
    pub relay: Option<String>,
    pub api: Option<String>,
    pub key: Option<String>,
    pub online: Option<String>,
}

lazy_static::lazy_static! {
    static ref SERVER_LATENCIES: std::sync::Mutex<std::collections::HashMap<String, i64>> = Default::default();
    static ref RESOLVED_PROFILES: std::sync::RwLock<std::collections::HashMap<String, ResolvedProfileData>> = Default::default();
}

pub fn update_resolved_profile_data(
    profile_id: &str,
    original_host: &str,
    resolved_host: &str,
    tcp_host: Option<&str>,
    relay: Option<&str>,
    api: Option<&str>,
    key: Option<&str>,
    online: Option<&str>,
) {
    let data = ResolvedProfileData {
        profile_id: profile_id.to_string(),
        original_host: original_host.to_string(),
        resolved_host: resolved_host.to_string(),
        tcp_host: tcp_host.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        relay: relay.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        api: api.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        key: key.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        online: online.filter(|s| !s.is_empty()).map(|s| s.to_string()),
    };

    let persisted = {
        match RESOLVED_PROFILES.write() {
            Ok(mut map) => {
                // Keys stay full ("host:port" / profile id): a bare-IP alias would let
                // profiles sharing one public IP overwrite each other. Bare-IP queries
                // are served by the scan fallback in get_resolved_profile_data.
                if !profile_id.is_empty() {
                    map.insert(profile_id.to_string(), data.clone());
                }
                if !original_host.is_empty() {
                    map.insert(original_host.to_string(), data.clone());
                }
                if !resolved_host.is_empty() {
                    map.insert(resolved_host.to_string(), data.clone());
                }
                if let Some(ref tcp) = data.tcp_host {
                    map.insert(tcp.clone(), data.clone());
                }
                if let Some(ref relay) = data.relay {
                    map.insert(relay.clone(), data.clone());
                }
                serde_json::to_string(&*map).ok()
            }
            Err(_) => None,
        }
    };
    if let Some(json) = persisted {
        Config::set_option("resolved-server-profiles".to_owned(), json);
    }
}

pub fn get_resolved_profile_data(host: &str) -> Option<ResolvedProfileData> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let p_host = parse_host(host);
    if let Ok(map) = RESOLVED_PROFILES.read() {
        if let Some(data) = map.get(host) {
            return Some(data.clone());
        }
        if !p_host.is_empty() {
            if let Some(data) = map.get(p_host) {
                return Some(data.clone());
            }
        }
        for data in map.values() {
            if is_host_match_resolved(data, host) {
                return Some(data.clone());
            }
        }
    }
    let raw = Config::get_option("resolved-server-profiles");
    if !raw.is_empty() {
        if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, ResolvedProfileData>>(&raw) {
            if let Some(data) = map.get(host) {
                return Some(data.clone());
            }
            if !p_host.is_empty() {
                if let Some(data) = map.get(p_host) {
                    return Some(data.clone());
                }
            }
            for data in map.values() {
                if is_host_match_resolved(data, host) {
                    return Some(data.clone());
                }
            }
        }
    }
    None
}

fn is_host_match_resolved(r: &ResolvedProfileData, h: &str) -> bool {
    let p = parse_host(h);
    if is_host_match_str(&r.original_host, h, p)
        || is_host_match_str(&r.resolved_host, h, p)
        || r.tcp_host.as_deref().map(|s| is_host_match_str(s, h, p)).unwrap_or(false)
        || r.relay.as_deref().map(|s| is_host_match_str(s, h, p)).unwrap_or(false)
    {
        return true;
    }
    false
}

fn is_host_match_str(target: &str, h: &str, p: &str) -> bool {
    let target = target.trim();
    let h = h.trim();
    if target.eq_ignore_ascii_case(h) {
        return true;
    }
    // Two entries that both spell out a port must agree on it: several profiles
    // can share one public IP behind different STUN-mapped ports.
    if has_explicit_port(target) && has_explicit_port(h) {
        return false;
    }
    let tp = parse_host(target);
    !tp.is_empty() && !p.is_empty() && tp.eq_ignore_ascii_case(p)
}

fn has_explicit_port(s: &str) -> bool {
    parse_host(s) != s
}

fn merge_resolved_into_profile(p: &mut ServerProfile, r: &ResolvedProfileData) {
    if !r.resolved_host.is_empty() {
        p.host = r.resolved_host.clone();
    }
    if let Some(ref tcp) = r.tcp_host {
        p.tcp_host = Some(tcp.clone());
    }
    if let Some(ref relay) = r.relay {
        p.relay = Some(relay.clone());
    }
    if let Some(ref key) = r.key {
        p.key = Some(key.clone());
    }
    if let Some(ref api) = r.api {
        p.api = Some(api.clone());
    }
    if let Some(ref online) = r.online {
        p.online = Some(online.clone());
    }
}

/// Helper to match server hosts with or without ports
pub fn is_host_match(h1: &str, h2: &str) -> bool {
    let h1 = h1.trim();
    let h2 = h2.trim();
    if h1.is_empty() || h2.is_empty() {
        return false;
    }
    let p1 = parse_host(h1);
    let p2 = parse_host(h2);
    if !p1.is_empty() && !p2.is_empty() {
        if p1.eq_ignore_ascii_case(p2) {
            return true;
        }
    }
    if h1.eq_ignore_ascii_case(h2) {
        return true;
    }
    if resolved_hosts_match(h1, h2) {
        return true;
    }
    false
}

/// `is_host_match` for two addresses that name the same kind of server, i.e. two rendezvous
/// hosts. Several profiles can share one public IP behind different STUN-mapped ports, and for
/// that comparison the port is part of the identity: without it `get_profile_by_host` hands out
/// the first profile's key, tcp host and relay, which surfaces as an endless key mismatch
/// against the right server.
///
/// Cross-role checks must keep using `is_host_match`: a `rendezvous-server-tcp` or `relay-server`
/// address belongs to the same machine on a different port by design.
pub fn is_same_rendezvous_host(h1: &str, h2: &str) -> bool {
    let h1 = h1.trim();
    let h2 = h2.trim();
    if h1.is_empty() || h2.is_empty() {
        return false;
    }
    if has_explicit_port(h1) && has_explicit_port(h2) {
        // Different spellings are still one server when a TXT resolution linked them.
        return h1.eq_ignore_ascii_case(h2) || resolved_hosts_match(h1, h2);
    }
    is_host_match(h1, h2)
}

fn resolved_hosts_match(h1: &str, h2: &str) -> bool {
    if let Some(r1) = get_resolved_profile_data(h1) {
        if is_host_match_resolved(&r1, h2) {
            return true;
        }
    }
    if let Some(r2) = get_resolved_profile_data(h2) {
        if is_host_match_resolved(&r2, h1) {
            return true;
        }
    }
    false
}

/// Get a server profile by host.
pub fn get_profile_by_host(host: &str) -> Option<ServerProfile> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    let profiles = get_server_profiles();
    for p in &profiles {
        if p.host.trim().is_empty() {
            continue;
        }
        if is_same_rendezvous_host(&p.host, host) {
            let mut res = p.clone();
            if let Some(resolved) = get_resolved_profile_data(host) {
                merge_resolved_into_profile(&mut res, &resolved);
            }
            return Some(res);
        }
        if let Some(ref tcp) = p.tcp_host {
            if is_host_match(tcp, host) {
                let mut res = p.clone();
                if let Some(resolved) = get_resolved_profile_data(host) {
                    merge_resolved_into_profile(&mut res, &resolved);
                }
                return Some(res);
            }
        }
        if let Some(ref relay) = p.relay {
            if is_host_match(relay, host) {
                let mut res = p.clone();
                if let Some(resolved) = get_resolved_profile_data(host) {
                    merge_resolved_into_profile(&mut res, &resolved);
                }
                return Some(res);
            }
        }
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        let base = profiles.into_iter().find(|p| {
            (!resolved.profile_id.is_empty() && p.id == resolved.profile_id)
                || is_same_rendezvous_host(&p.host, &resolved.original_host)
        });
        let mut p = base.unwrap_or_else(|| ServerProfile {
            id: if !resolved.profile_id.is_empty() { resolved.profile_id.clone() } else { "resolved".to_string() },
            name: resolved.original_host.clone(),
            host: resolved.resolved_host.clone(),
            enabled: true,
            ..Default::default()
        });
        merge_resolved_into_profile(&mut p, &resolved);
        return Some(p);
    }
    None
}

/// Get key associated with a specific host.
pub fn get_key_by_host(host: &str) -> String {
    let host = host.trim();
    if host.is_empty() {
        return Config::get_option("key");
    }
    if is_official_server(host) {
        return hbb_common::config::RS_PUB_KEY.to_string();
    }
    if let Some(p) = get_profile_by_host(host) {
        if let Some(ref key) = p.key {
            if !key.is_empty() {
                return key.clone();
            }
        }
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        if let Some(ref key) = resolved.key {
            if !key.is_empty() {
                return key.clone();
            }
        }
    }
    Config::get_option("key")
}

/// Get TCP host associated with a specific host.
pub fn get_tcp_host_by_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || is_official_server(host) {
        return None;
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        if let Some(ref tcp) = resolved.tcp_host {
            if !tcp.is_empty() {
                return Some(tcp.clone());
            }
        }
    }
    get_profile_by_host(host).and_then(|p| p.tcp_host)
}

/// Get relay associated with a specific host.
pub fn get_relay_by_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || is_official_server(host) {
        return None;
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        if let Some(ref relay) = resolved.relay {
            if !relay.is_empty() {
                return Some(relay.clone());
            }
        }
    }
    get_profile_by_host(host).and_then(|p| p.relay)
}

/// Get API associated with a specific host.
pub fn get_api_by_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        if let Some(ref api) = resolved.api {
            if !api.is_empty() {
                return Some(api.clone());
            }
        }
    }
    get_profile_by_host(host).and_then(|p| p.api)
}

/// Get Online/Status server associated with a specific host.
pub fn get_online_by_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || is_official_server(host) {
        return None;
    }
    if let Some(resolved) = get_resolved_profile_data(host) {
        if let Some(ref online) = resolved.online {
            if !online.is_empty() {
                return Some(online.clone());
            }
        }
    }
    get_profile_by_host(host).and_then(|p| p.online)
}

/// Derive the default API server URL from a rendezvous host, mirroring the
/// port-2 fallback of `get_api_server_` in src/common.rs.
fn derive_api_from_host(host: &str) -> String {
    let host = host.trim();
    if host.is_empty() {
        return String::new();
    }
    let s = hbb_common::socket_client::increase_port(host, -2);
    if s == host {
        format!("http://{}:{}", s, hbb_common::config::RENDEZVOUS_PORT - 2)
    } else {
        format!("http://{}", s)
    }
}

fn api_host_port(api: &str) -> (String, Option<u16>) {
    let s = api.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.trim_end_matches('/');
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let port = rest[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok());
            return (rest[..end].to_string(), port);
        }
    }
    if s.matches(':').count() > 1 {
        // Bare IPv6 literal without brackets.
        return (s.to_string(), None);
    }
    match s.rfind(':') {
        Some(idx) if s[idx + 1..].chars().all(|c| c.is_ascii_digit()) && !s[idx + 1..].is_empty() => {
            (s[..idx].to_string(), s[idx + 1..].parse::<u16>().ok())
        }
        _ => (s.to_string(), None),
    }
}

fn api_host_port_eq(a: &(String, Option<u16>), b: &(String, Option<u16>)) -> bool {
    if !a.0.eq_ignore_ascii_case(&b.0) {
        return false;
    }
    match (a.1, b.1) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// Find the profile that owns the given api-server URL: the profile whose
/// effective api (explicit, resolved, or derived from its rendezvous host)
/// names the same host and port.
pub fn get_profile_by_api(api: &str) -> Option<ServerProfile> {
    let target = api_host_port(api);
    if target.0.is_empty() {
        return None;
    }
    for p in get_server_profiles() {
        if p.host.trim().is_empty() {
            continue;
        }
        let eff = get_api_by_host(&p.host)
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| derive_api_from_host(&p.host));
        if !eff.is_empty() && api_host_port_eq(&api_host_port(&eff), &target) {
            return Some(p);
        }
    }
    None
}

// The legacy global `access_token`/`user_info` options stay as the mirror of the
// currently active server's login, so existing single-server consumers keep
// working; profile entries hold the per-server state.
pub fn has_persisted_profiles() -> bool {
    !Config::get_option("server-profiles").is_empty()
}

fn migrate_legacy_login_state() {
    use std::sync::OnceLock;
    static MIGRATED: OnceLock<()> = OnceLock::new();
    if MIGRATED.get().is_some() || Status::get("login_state_migrated") == "Y" {
        MIGRATED.set(()).ok();
        return;
    }
    let token = LocalConfig::get_option("access_token");
    if !token.is_empty() && has_persisted_profiles() {
        let api = Config::get_option("api-server");
        let custom = Config::get_option("custom-rendezvous-server");
        let cur_api = if !api.is_empty() {
            api
        } else {
            derive_api_from_host(&custom)
        };
        if !cur_api.is_empty() {
            if let Some(p) = get_profile_by_api(&cur_api) {
                if p.access_token.as_deref().unwrap_or("").is_empty() {
                    let mut profiles = get_server_profiles();
                    if let Some(entry) = profiles.iter_mut().find(|e| e.id == p.id) {
                        entry.access_token = Some(token);
                        entry.user_info = Some(LocalConfig::get_option("user_info"));
                        set_server_profiles(&profiles);
                    }
                }
            }
        }
    }
    Status::set("login_state_migrated", "Y".to_owned());
    MIGRATED.set(()).ok();
}

/// Read the login state belonging to the given api-server: the owning
/// profile's token when there is one, else the legacy global slot.
pub fn get_login_by_api(api: &str) -> (String, String) {
    migrate_legacy_login_state();
    if let Some(p) = get_profile_by_api(api) {
        if let Some(ref t) = p.access_token {
            if !t.is_empty() {
                return (t.clone(), p.user_info.clone().unwrap_or_default());
            }
        }
    }
    (
        LocalConfig::get_option("access_token"),
        LocalConfig::get_option("user_info"),
    )
}

/// Login state of the server identified by its rendezvous host. Callers that
/// know only the host (a cross-server peer) get that server's own login; the
/// legacy global slot answers when no profile owns the host.
pub fn get_login_by_host(host: &str) -> (String, String) {
    let api = get_api_by_host(host)
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| derive_api_from_host(host));
    get_login_by_api(&api)
}

/// Store a login for the given api-server. The owning profile entry takes the
/// token; when that profile is the active server the legacy global slot is
/// mirrored too. Without persisted profiles (non-profile mode) only the global
/// slot is written, so existing single-server setups behave as before.
pub fn set_login_by_api(api: &str, access_token: &str, user_info: &str) {
    migrate_legacy_login_state();
    if has_persisted_profiles() {
        if let Some(p) = get_profile_by_api(api) {
            let mut profiles = get_server_profiles();
            if let Some(entry) = profiles.iter_mut().find(|e| e.id == p.id) {
                entry.access_token = Some(access_token.to_string());
                entry.user_info = Some(user_info.to_string());
                let custom = Config::get_option("custom-rendezvous-server");
                let is_active =
                    !custom.is_empty() && is_same_rendezvous_host(&entry.host, &custom);
                set_server_profiles(&profiles);
                if is_active {
                    LocalConfig::set_option(
                        "access_token".to_owned(),
                        access_token.to_string(),
                    );
                    LocalConfig::set_option("user_info".to_owned(), user_info.to_string());
                }
                return;
            }
        }
    }
    LocalConfig::set_option("access_token".to_owned(), access_token.to_string());
    LocalConfig::set_option("user_info".to_owned(), user_info.to_string());
}

/// Update only the user_info of the given api-server's login, leaving its
/// token untouched; mirrors to the global slot when that profile is active.
pub fn update_login_user_by_api(api: &str, user_info: &str) {
    if has_persisted_profiles() {
        if let Some(p) = get_profile_by_api(api) {
            let mut profiles = get_server_profiles();
            let custom = Config::get_option("custom-rendezvous-server");
            if let Some(entry) = profiles.iter_mut().find(|e| e.id == p.id) {
                let is_active =
                    !custom.is_empty() && is_same_rendezvous_host(&entry.host, &custom);
                entry.user_info = Some(user_info.to_string());
                set_server_profiles(&profiles);
                if is_active {
                    LocalConfig::set_option("user_info".to_owned(), user_info.to_string());
                }
                return;
            }
        }
    }
    LocalConfig::set_option("user_info".to_owned(), user_info.to_string());
}

/// Drop the login state of the given api-server: the owning profile entry,
/// and the global slot too when that profile is the active server.
pub fn clear_login_by_api(api: &str) {
    if has_persisted_profiles() {
        if let Some(p) = get_profile_by_api(api) {
            let mut profiles = get_server_profiles();
            let custom = Config::get_option("custom-rendezvous-server");
            if let Some(entry) = profiles.iter_mut().find(|e| e.id == p.id) {
                let is_active =
                    !custom.is_empty() && is_same_rendezvous_host(&entry.host, &custom);
                entry.access_token = None;
                entry.user_info = None;
                set_server_profiles(&profiles);
                if is_active {
                    LocalConfig::set_option("access_token".to_owned(), String::new());
                    LocalConfig::set_option("user_info".to_owned(), String::new());
                }
                return;
            }
        }
    }
    LocalConfig::set_option("access_token".to_owned(), String::new());
    LocalConfig::set_option("user_info".to_owned(), String::new());
}

/// Re-point the legacy global login slot at the currently active profile, so
/// single-server consumers read the right server's login after a switch. No-op
/// without persisted profiles: the global slot is already authoritative there.
pub fn sync_login_mirror() {
    if !has_persisted_profiles() {
        return;
    }
    let custom = Config::get_option("custom-rendezvous-server");
    let mut token = String::new();
    let mut user_info = String::new();
    if !custom.is_empty() {
        if let Some(p) = get_server_profiles()
            .iter()
            .find(|p| !p.host.is_empty() && is_same_rendezvous_host(&p.host, &custom))
        {
            token = p.access_token.clone().unwrap_or_default();
            user_info = p.user_info.clone().unwrap_or_default();
        }
    }
    LocalConfig::set_option("access_token".to_owned(), token);
    LocalConfig::set_option("user_info".to_owned(), user_info);
}

pub fn update_server_profile_latency(id: &str, configured_host: &str, resolved_host: &str, latency: i64) {
    // The JSON is built under the lock but written to disk after it is released:
    // Config::set_option reads and rewrites the options file synchronously, and
    // the mediator calls here from a Tokio task.
    let persisted = match SERVER_LATENCIES.lock() {
        Ok(mut map) => {
            if !id.is_empty() {
                map.insert(id.to_string(), latency);
            }
            if !configured_host.is_empty() {
                map.insert(configured_host.to_string(), latency);
            }
            if !resolved_host.is_empty() {
                map.insert(resolved_host.to_string(), latency);
            }
            serde_json::to_string(&*map).ok()
        }
        Err(_) => None,
    };
    if let Some(json) = persisted {
        Config::set_option("server-latencies".to_owned(), json);
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
            if is_same_rendezvous_host(k, host) {
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
                if is_same_rendezvous_host(k, host) {
                    return *v;
                }
            }
        }
    }
    // Fallback: check hbb_common online state for official / global server
    let online_state = hbb_common::config::get_online_state();
    if online_state > 0 {
        return online_state;
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
    pub logged_in: bool,
}

pub fn get_server_profile_statuses() -> Vec<ServerProfileStatus> {
    let mut profiles = get_server_profiles();
    let has_official = profiles.iter().any(|p| is_official_server(&p.host));
    if !has_official {
        let active_profiles = get_active_server_profiles();
        let enabled = active_profiles.is_empty() || active_profiles.iter().any(|p| is_official_server(&p.host));
        let lat_us = hbb_common::config::get_online_state();
        let cur_server = Config::get_rendezvous_server();
        let is_cur_official = cur_server.is_empty() || is_official_server(&cur_server);
        let online = enabled && is_cur_official && lat_us > 0;
        let latency_ms = if online { (lat_us + 999) / 1000 } else { -1 };
        profiles.push(ServerProfile {
            id: "official".to_string(),
            name: "官方服务器".to_string(),
            host: if is_cur_official && !cur_server.is_empty() { cur_server } else { "public".to_string() },
            enabled,
            ..Default::default()
        });
    }
    profiles
        .into_iter()
        .map(|p| {
            let is_off = is_official_server(&p.host);
            let lat_us = if is_off {
                hbb_common::config::get_online_state()
            } else {
                get_server_latency_by_profile(&p.id, &p.host)
            };
            let online = p.enabled && lat_us > 0;
            let latency_ms = if lat_us > 0 { (lat_us + 999) / 1000 } else { -1 };
            // The synthesized official row has no profile entry. Its host is the
            // "public" placeholder exactly when a custom server is active, and
            // then the global slot describes that custom server, not official.
            let logged_in = if p.host == "public" {
                Config::get_option("custom-rendezvous-server").is_empty()
                    && !LocalConfig::get_option("access_token").is_empty()
            } else {
                let eff_api = get_api_by_host(&p.host)
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| derive_api_from_host(&p.host));
                !eff_api.is_empty() && !get_login_by_api(&eff_api).0.is_empty()
            };
            ServerProfileStatus {
                id: p.id,
                name: p.name,
                host: p.host,
                enabled: p.enabled,
                online,
                latency_ms,
                logged_in,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ported_entries_only_match_the_same_port() {
        // Same public IP, different STUN-mapped ports must not cross-match.
        assert!(!is_host_match_str("1.2.3.4:24439", "1.2.3.4:24869", "1.2.3.4"));
        assert!(is_host_match_str("1.2.3.4:24439", "1.2.3.4:24439", "1.2.3.4"));
    }

    #[test]
    fn same_rendezvous_host_keeps_distinct_ports_apart() {
        assert!(!is_same_rendezvous_host("203.0.113.7:17366", "203.0.113.7:17368"));
        assert!(is_same_rendezvous_host("203.0.113.7:17366", "203.0.113.7:17366"));
        assert!(is_same_rendezvous_host("203.0.113.7", "203.0.113.7:17366"));
        assert!(is_same_rendezvous_host("203.0.113.7:17366", "203.0.113.7"));
        assert!(!is_same_rendezvous_host("[2001:db8::7]:17366", "[2001:db8::7]:17368"));
    }

    #[test]
    fn cross_role_host_match_ignores_the_port() {
        // A rendezvous-server-tcp / relay-server option names the same machine on another
        // port, so it must still be recognised as belonging to that server.
        assert!(is_host_match("203.0.113.7:21115", "203.0.113.7:21116"));
        assert!(is_host_match("203.0.113.7:21117", "203.0.113.7"));
    }

    #[test]
    fn bare_queries_still_match_by_host() {
        assert!(is_host_match_str("1.2.3.4:24439", "1.2.3.4", "1.2.3.4"));
        assert!(is_host_match_str("rd.example.com", "rd.example.com:21116", "rd.example.com"));
    }

    #[test]
    fn ipv6_without_port_is_not_treated_as_ported() {
        assert!(!has_explicit_port("2408::1"));
        assert!(has_explicit_port("[2408::1]:21116"));
        assert!(!is_host_match_str("[2408::1]:21116", "[2408::1]:22222", "2408::1"));
        assert!(is_host_match_str("[2408::1]:21116", "2408::1", "2408::1"));
    }
}

