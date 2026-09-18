use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

lazy_static::lazy_static! {
    /// input -> (resolved config or negative result, cached at)
    static ref TXT_CACHE: RwLock<HashMap<String, (Option<ResolvedServerConfig>, Instant)>> =
        Default::default();
}

const TXT_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedServerConfig {
    pub host: String,
    pub tcp: Option<String>,
    pub relay: Option<String>,
    pub api: Option<String>,
    pub key: Option<String>,
    pub online: Option<String>,
}

/// Decode RustDesk exported configuration string (reversed base64 of json)
fn decode_rustdesk_config(raw: &str) -> Option<ResolvedServerConfig> {
    let reversed: String = raw.trim().chars().rev().collect();
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        reversed.as_bytes(),
    )
    .or_else(|_| {
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            reversed.as_bytes(),
        )
    })
    .ok()?;

    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let host = json.get("host").and_then(|v| v.as_str())?.to_string();
    let tcp = json
        .get("tcp")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let relay = json
        .get("relay")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let api = json
        .get("api")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let key = json
        .get("key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let online = json
        .get("online")
        .or_else(|| json.get("status"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(ResolvedServerConfig {
        host,
        tcp,
        relay,
        api,
        key,
        online,
    })
}

/// Parse raw text (from TXT record or HTTP text) into ResolvedServerConfig
pub fn parse_txt_content(raw: &str) -> Option<ResolvedServerConfig> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let raw = raw.trim_matches('"').trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(cfg) = decode_rustdesk_config(raw) {
        return Some(cfg);
    }

    if raw.contains("host=") {
        let mut cfg = ResolvedServerConfig::default();
        let parts = raw.split(&[',', ';', '\n'][..]);
        for part in parts {
            let part = part.trim();
            if let Some((k, v)) = part.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"');
                match k {
                    "host" => cfg.host = v.to_string(),
                    "tcp" | "host_tcp" | "signaling" => cfg.tcp = Some(v.to_string()),
                    "relay" => cfg.relay = Some(v.to_string()),
                    "api" => cfg.api = Some(v.to_string()),
                    "key" => cfg.key = Some(v.to_string()),
                    "online" | "status" => cfg.online = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        if !cfg.host.is_empty() {
            return Some(cfg);
        }
    }

    if looks_like_host_port(raw) {
        return Some(ResolvedServerConfig {
            host: raw.to_string(),
            tcp: None,
            relay: None,
            api: None,
            key: None,
            online: None,
        });
    }
    None
}

/// Accept bare TXT content as a server address only when it is shaped like
/// `host` or `host:port`. Unrelated records (SPF, DKIM, domain validation)
/// carry spaces, '=' or ';' and fail this check.
fn looks_like_host_port(s: &str) -> bool {
    let host = match s.rsplit_once(':') {
        Some((h, p)) => {
            if p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            h
        }
        None => s,
    };
    if host.is_empty() || !host.contains('.') {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// Read a DNS name at `pos`, following compression pointers.
/// Returns the lowercase dotted name and the offset just past the name field.
fn read_dns_name(buf: &[u8], pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<&str> = Vec::new();
    let mut cur = pos;
    let mut end: Option<usize> = None;
    loop {
        if cur >= buf.len() {
            return None;
        }
        let len = buf[cur] as usize;
        if len == 0 {
            if end.is_none() {
                end = Some(cur + 1);
            }
            break;
        }
        if (len & 0xc0) == 0xc0 {
            if cur + 1 >= buf.len() {
                return None;
            }
            if end.is_none() {
                end = Some(cur + 2);
            }
            let ptr = ((len & 0x3f) as usize) << 8 | buf[cur + 1] as usize;
            // Compression pointers in a response always point backwards,
            // so requiring a strict decrease rules out pointer loops.
            if ptr >= cur {
                return None;
            }
            cur = ptr;
            continue;
        }
        let label_end = cur + 1 + len;
        if label_end > buf.len() {
            return None;
        }
        labels.push(std::str::from_utf8(&buf[cur + 1..label_end]).ok()?);
        cur = label_end;
    }
    Some((labels.join(".").to_ascii_lowercase(), end?))
}

/// Decode TXT RDATA (a series of `<length-byte><characters>` chunks).
fn read_txt_rdata(buf: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let len = buf[pos] as usize;
        pos += 1;
        if pos + len > buf.len() {
            return None;
        }
        parts.push(std::str::from_utf8(&buf[pos..pos + len]).ok()?);
        pos += len;
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(""))
}

/// Parse a DNS TXT response. The header must echo `query_id` and the question
/// must be for `domain`; only answers whose owner name matches are accepted.
fn parse_dns_txt_response(buf: &[u8], query_id: u16, domain: &str) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != query_id {
        return None;
    }
    // QR bit set and RCODE == NOERROR
    if buf[2] & 0x80 == 0 || buf[3] & 0x0f != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if qdcount == 0 || ancount == 0 {
        return None;
    }

    let (qname, mut pos) = read_dns_name(buf, 12)?;
    if qname != domain.to_ascii_lowercase() {
        return None;
    }
    pos += 4; // QTYPE (2) + QCLASS (2)
    if pos > buf.len() {
        return None;
    }

    for _ in 0..ancount {
        let (aname, next) = match read_dns_name(buf, pos) {
            Some(v) => v,
            None => break,
        };
        pos = next;
        if pos + 10 > buf.len() {
            break;
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > buf.len() {
            break;
        }
        if rtype == 16 && aname == qname {
            if let Some(txt) = read_txt_rdata(&buf[pos..pos + rdlen]) {
                return Some(txt);
            }
        }
        pos += rdlen;
    }

    None
}

/// One UDP TXT attempt against a single resolver.
async fn query_dns_txt_udp_one(
    srv: &str,
    packet: &[u8],
    query_id: u16,
    domain: &str,
) -> Option<String> {
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    if sock.connect(srv).await.is_err() {
        return None;
    }
    if sock.send(packet).await.is_err() {
        return None;
    }
    let mut buf = [0u8; 1024];
    let n = match tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf)).await {
        Ok(Ok(n)) => n,
        _ => return None,
    };
    let txt = parse_dns_txt_response(&buf[..n], query_id, domain)?;
    let cleaned = txt.trim().trim_matches('"').to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Query DNS TXT record via standard UDP DNS (port 53), racing all resolvers.
pub async fn query_dns_txt_udp(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return None;
    }

    // Per-query id keeps spoofers from replaying against a known fixed id;
    // the response must echo it (checked in parse_dns_txt_response).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0x1234);
    static DNS_ID_SEQ: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let seq = DNS_ID_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let query_id: u16 = (nanos as u16) ^ ((nanos >> 16) as u16) ^ seq.rotate_left(5);
    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&query_id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00]); // Standard query, RD=1
    packet.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // 1 question

    for label in domain.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0x00);
    packet.extend_from_slice(&[0x00, 0x10]); // QTYPE: TXT (16)
    packet.extend_from_slice(&[0x00, 0x01]); // QCLASS: IN (1)

    let dns_servers = [
        "223.5.5.5:53",
        "119.29.29.29:53",
        "180.76.76.76:53",
        "1.1.1.1:53",
        "8.8.8.8:53",
    ];

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for srv in dns_servers {
        let tx = tx.clone();
        let packet = packet.clone();
        let domain = domain.to_string();
        tokio::spawn(async move {
            let attempt = query_dns_txt_udp_one(srv, &packet, query_id, &domain);
            let res = tokio::time::timeout(Duration::from_secs(3), attempt)
                .await
                .unwrap_or(None);
            let _ = tx.send(res);
        });
    }
    drop(tx);
    while let Some(res) = rx.recv().await {
        if res.is_some() {
            return res;
        }
    }
    None
}

/// One DoH TXT attempt against a single endpoint.
async fn query_dns_txt_doh_one(
    client: &reqwest::Client,
    url: &str,
    domain: &str,
) -> Option<String> {
    let resp = client
        .get(url)
        .header("accept", "application/dns-json")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let val = resp.json::<serde_json::Value>().await.ok()?;
    let status = val
        .get("Status")
        .or_else(|| val.get("status"))
        .and_then(|s| s.as_i64())
        .unwrap_or(-1);
    if status != 0 {
        return None;
    }
    let answers = val.get("Answer").or_else(|| val.get("answer")).and_then(|a| a.as_array())?;
    for ans in answers {
        let type_ = ans.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
        if type_ != 16 {
            continue;
        }
        let name = ans.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if !name.trim_end_matches('.').eq_ignore_ascii_case(domain) {
            continue;
        }
        if let Some(data) = ans.get("data").and_then(|d| d.as_str()) {
            let cleaned = data.trim().trim_matches('"').to_string();
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }
    None
}

/// Query DNS TXT record via DoH (DNS-over-HTTPS), racing all endpoints.
pub async fn query_dns_txt_doh(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return None;
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return None,
    };

    let doh_urls = [
        format!("https://doh.pub/resolve?name={domain}&type=16"),
        format!("https://1.1.1.1/dns-query?name={domain}&type=16"),
        format!("https://dns.alidns.com/resolve?name={domain}&type=16"),
        format!("https://dns.google/resolve?name={domain}&type=16"),
    ];

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for url in doh_urls {
        let tx = tx.clone();
        let client = client.clone();
        let domain = domain.to_string();
        tokio::spawn(async move {
            let res = tokio::time::timeout(Duration::from_secs(4), query_dns_txt_doh_one(&client, &url, &domain))
                .await
                .unwrap_or(None);
            let _ = tx.send(res);
        });
    }
    drop(tx);
    while let Some(res) = rx.recv().await {
        if res.is_some() {
            return res;
        }
    }
    None
}

/// Query DNS TXT record using UDP first, then fallback to DoH
pub async fn query_dns_txt(domain: &str) -> Option<String> {
    if let Some(txt) = query_dns_txt_udp(domain).await {
        return Some(txt);
    }
    query_dns_txt_doh(domain).await
}

/// Fetch remote txt file content via HTTP/HTTPS, capped at 64 KiB.
pub async fn fetch_http_txt(url: &str) -> Option<String> {
    const MAX_HTTP_TXT_BYTES: usize = 64 * 1024;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let mut resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    if resp.content_length().unwrap_or(0) > MAX_HTTP_TXT_BYTES as u64 {
        return None;
    }
    let mut out: Vec<u8> = Vec::new();
    while let Some(part) = resp.chunk().await.ok()? {
        if out.len() + part.len() > MAX_HTTP_TXT_BYTES {
            return None;
        }
        out.extend_from_slice(&part);
    }
    String::from_utf8(out).ok()
}

/// Resolve server configuration from user input string, with a short-lived
/// cache so hot paths (get_rendezvous_server, mediator ticks) do not re-issue
/// network queries. Negative results are cached too.
/// 1. "http://..." or "https://..." -> Fetch URL content
/// 2. "txt:domain.com" -> Query TXT record explicitly
/// 3. "domain.com" or "domain.com:port" -> Query TXT record for domain; if present, use it
pub async fn resolve_server_config(input: &str) -> Option<ResolvedServerConfig> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(cached) = txt_cache_get(input) {
        return cached;
    }
    let resolved = resolve_server_config_uncached(input).await;
    txt_cache_put(input, resolved.clone());
    resolved
}

fn txt_cache_get(key: &str) -> Option<Option<ResolvedServerConfig>> {
    if let Ok(map) = TXT_CACHE.read() {
        if let Some((cfg, at)) = map.get(key) {
            if at.elapsed() < TXT_CACHE_TTL {
                return Some(cfg.clone());
            }
        }
    }
    None
}

fn txt_cache_put(key: &str, cfg: Option<ResolvedServerConfig>) {
    if let Ok(mut map) = TXT_CACHE.write() {
        if map.len() > 128 {
            map.clear();
        }
        map.insert(key.to_string(), (cfg, Instant::now()));
    }
}

async fn resolve_server_config_uncached(input: &str) -> Option<ResolvedServerConfig> {
    if input.starts_with("http://") || input.starts_with("https://") {
        if let Some(content) = fetch_http_txt(input).await {
            return parse_txt_content(&content);
        }
        return None;
    }

    if let Some(domain) = input.strip_prefix("txt:") {
        let domain = domain.split(':').next().unwrap_or(domain).trim();
        if let Some(txt) = query_dns_txt(domain).await {
            return parse_txt_content(&txt);
        }
        return None;
    }

    // Official servers never carry TXT configs for this feature; querying
    // them risks mistaking unrelated TXT records (SPF/DKIM) for a host.
    let host = input.split(':').next().unwrap_or(input).trim();
    if !host.is_empty()
        && host.contains('.')
        && host.parse::<std::net::IpAddr>().is_err()
        && !crate::server_profile::is_official_server(host)
    {
        if let Some(txt) = query_dns_txt(host).await {
            if let Some(cfg) = parse_txt_content(&txt) {
                return Some(cfg);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns_name_bytes(name: &str) -> Vec<u8> {
        let mut v = Vec::new();
        for label in name.split('.') {
            v.push(label.len() as u8);
            v.extend_from_slice(label.as_bytes());
        }
        v.push(0);
        v
    }

    fn dns_txt_rr(owner: &[u8], txt: &str) -> Vec<u8> {
        let mut v = owner.to_vec();
        v.extend_from_slice(&[0x00, 0x10, 0x00, 0x01]); // TXT, IN
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x3c]); // TTL
        v.extend_from_slice(&((txt.len() + 1) as u16).to_be_bytes());
        v.push(txt.len() as u8);
        v.extend_from_slice(txt.as_bytes());
        v
    }

    #[test]
    fn test_parse_simple_txt() {
        let content = "\"rustdesk.6143443.xyz:22111\"";
        let res = parse_txt_content(content).expect("failed to parse");
        assert_eq!(res.host, "rustdesk.6143443.xyz:22111");
        assert_eq!(res.relay, None);
    }

    #[test]
    fn test_parse_unrelated_txt_rejected() {
        // SPF, DKIM and site-verification records must never be mistaken for a host.
        assert!(parse_txt_content("v=spf1 include:_spf.gmail.com ~all").is_none());
        assert!(parse_txt_content("v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQ==").is_none());
        assert!(parse_txt_content("google-site-verification=AbCdEf123456").is_none());
        assert!(parse_txt_content("just some random prose").is_none());
    }

    #[test]
    fn test_parse_bare_domain_txt_accepted() {
        let res = parse_txt_content("rd.example.com").expect("failed to parse");
        assert_eq!(res.host, "rd.example.com");
    }

    #[test]
    fn test_parse_kv_txt() {
        let content = "host=1.2.3.4:22111,relay=1.2.3.4:22112,key=mysecretkey";
        let res = parse_txt_content(content).expect("failed to parse");
        assert_eq!(res.host, "1.2.3.4:22111");
        assert_eq!(res.tcp, None);
        assert_eq!(res.relay, Some("1.2.3.4:22112".to_string()));
        assert_eq!(res.key, Some("mysecretkey".to_string()));
    }

    #[test]
    fn test_parse_stun_multi_port_txt() {
        let content = "host=198.51.100.123:24869,tcp=198.51.100.123:24439,relay=198.51.100.123:24867,key=uSmsZQFJuhdstRBFhFXYaO0yprAr5wVy1rI+iuzNKEo=";
        let res = parse_txt_content(content).expect("failed to parse");
        assert_eq!(res.host, "198.51.100.123:24869");
        assert_eq!(res.tcp, Some("198.51.100.123:24439".to_string()));
        assert_eq!(res.relay, Some("198.51.100.123:24867".to_string()));
        assert_eq!(res.key, Some("uSmsZQFJuhdstRBFhFXYaO0yprAr5wVy1rI+iuzNKEo=".to_string()));
    }

    #[test]
    fn test_parse_dns_txt_response_validates_id_and_owner() {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&0x1234u16.to_be_bytes());
        pkt.extend_from_slice(&[0x81, 0x80]); // response, NOERROR
        pkt.extend_from_slice(&[0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00]);
        pkt.extend_from_slice(&dns_name_bytes("rd.example.com"));
        pkt.extend_from_slice(&[0x00, 0x10, 0x00, 0x01]); // question TXT/IN
        // First answer: an unrelated owner (e.g. an SPF record) -> must be skipped.
        let spf = "v=spf1 include:_spf.example.com ~all";
        pkt.extend_from_slice(&dns_txt_rr(&dns_name_bytes("example.com"), spf));
        // Second answer: compressed pointer to the question name -> the real record.
        pkt.extend_from_slice(&dns_txt_rr(&[0xc0, 0x0c], "host=1.2.3.4:21116"));

        let txt = parse_dns_txt_response(&pkt, 0x1234, "rd.example.com");
        assert_eq!(txt.as_deref(), Some("host=1.2.3.4:21116"));
        // Wrong transaction id or wrong queried domain must be rejected.
        assert!(parse_dns_txt_response(&pkt, 0x4321, "rd.example.com").is_none());
        assert!(parse_dns_txt_response(&pkt, 0x1234, "example.com").is_none());
    }

    #[tokio::test]
    async fn test_resolve_server_config_ip_skipped() {
        assert!(resolve_server_config("127.0.0.1").await.is_none());
        assert!(resolve_server_config("127.0.0.1:21116").await.is_none());
        assert!(resolve_server_config("198.51.100.123:22443").await.is_none());
    }

    #[tokio::test]
    async fn test_resolve_server_config_official_skipped() {
        // Official servers are never TXT-resolved, even if TXT records exist.
        assert!(resolve_server_config("rs-web.rustdesk.com").await.is_none());
        assert!(resolve_server_config("public").await.is_none());
    }

    #[tokio::test]
    #[ignore]
    async fn test_query_user_domain() {
        let res = query_dns_txt("rustdesk.6143443.xyz").await;
        if let Some(val) = res {
            println!("Resolved TXT value: {}", val);
            let cfg = parse_txt_content(&val).expect("failed to parse txt");
            assert!(!cfg.host.is_empty());
        }
    }
}
