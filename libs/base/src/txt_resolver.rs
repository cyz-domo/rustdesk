use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedServerConfig {
    pub host: String,
    pub relay: Option<String>,
    pub api: Option<String>,
    pub key: Option<String>,
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

    Some(ResolvedServerConfig {
        host,
        relay,
        api,
        key,
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
                    "relay" => cfg.relay = Some(v.to_string()),
                    "api" => cfg.api = Some(v.to_string()),
                    "key" => cfg.key = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        if !cfg.host.is_empty() {
            return Some(cfg);
        }
    }

    Some(ResolvedServerConfig {
        host: raw.to_string(),
        relay: None,
        api: None,
        key: None,
    })
}

/// Parse TXT RDATA from a DNS response packet
fn parse_dns_txt_response(buf: &[u8], _domain: &str) -> Option<String> {
    if buf.len() < 12 {
        return None;
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let ancount = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    if ancount == 0 {
        return None;
    }

    let mut pos = 12;
    // Skip Question Section
    for _ in 0..qdcount {
        while pos < buf.len() {
            let len = buf[pos] as usize;
            if len == 0 {
                pos += 1;
                break;
            } else if (len & 0xc0) == 0xc0 {
                pos += 2;
                break;
            } else {
                pos += 1 + len;
            }
        }
        pos += 4; // QTYPE (2) + QCLASS (2)
        if pos > buf.len() {
            return None;
        }
    }

    // Parse Answer Section
    for _ in 0..ancount {
        if pos >= buf.len() {
            break;
        }
        // Skip Name
        while pos < buf.len() {
            let len = buf[pos] as usize;
            if len == 0 {
                pos += 1;
                break;
            } else if (len & 0xc0) == 0xc0 {
                pos += 2;
                break;
            } else {
                pos += 1 + len;
            }
        }
        if pos + 10 > buf.len() {
            break;
        }
        let rtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let _rclass = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);
        let _ttl = u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        let rdlen = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]) as usize;
        pos += 10;

        if pos + rdlen > buf.len() {
            break;
        }

        if rtype == 16 {
            // TXT record: series of <length-byte><characters>
            let mut txt_parts = Vec::new();
            let mut rpos = pos;
            let rend = pos + rdlen;
            while rpos < rend {
                let slen = buf[rpos] as usize;
                rpos += 1;
                if rpos + slen <= rend {
                    if let Ok(s) = std::str::from_utf8(&buf[rpos..rpos + slen]) {
                        txt_parts.push(s);
                    }
                    rpos += slen;
                } else {
                    break;
                }
            }
            if !txt_parts.is_empty() {
                return Some(txt_parts.join(""));
            }
        }
        pos += rdlen;
    }

    None
}

/// Query DNS TXT record via standard UDP DNS (port 53)
pub async fn query_dns_txt_udp(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return None;
    }

    let mut packet = Vec::with_capacity(64);
    packet.extend_from_slice(&[0x12, 0x34]); // ID
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

    for srv in dns_servers {
        let sock = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
            Ok(s) => s,
            Err(_) => continue,
        };
        if sock.connect(srv).await.is_err() {
            continue;
        }
        if sock.send(&packet).await.is_err() {
            continue;
        }
        let mut buf = [0u8; 1024];
        let n = match tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => continue,
        };

        if let Some(txt) = parse_dns_txt_response(&buf[..n], domain) {
            let cleaned = txt.trim().trim_matches('"').to_string();
            if !cleaned.is_empty() {
                return Some(cleaned);
            }
        }
    }

    None
}

/// Query DNS TXT record via DoH (DNS-over-HTTPS)
pub async fn query_dns_txt_doh(domain: &str) -> Option<String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return None;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;

    let doh_urls = [
        format!("https://doh.pub/resolve?name={domain}&type=16"),
        format!("https://1.1.1.1/dns-query?name={domain}&type=16"),
        format!("https://dns.alidns.com/resolve?name={domain}&type=16"),
        format!("https://dns.google/resolve?name={domain}&type=16"),
    ];

    for url in doh_urls {
        let resp = match client
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };

        if !resp.status().is_success() {
            continue;
        }

        if let Ok(val) = resp.json::<serde_json::Value>().await {
            let status = val.get("Status").or_else(|| val.get("status")).and_then(|s| s.as_i64()).unwrap_or(-1);
            if status == 0 {
                let answers = val.get("Answer").or_else(|| val.get("answer")).and_then(|a| a.as_array());
                if let Some(answers) = answers {
                    for ans in answers {
                        let type_ = ans.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
                        if type_ == 16 {
                            if let Some(data) = ans.get("data").and_then(|d| d.as_str()) {
                                let cleaned = data.trim().trim_matches('"').to_string();
                                if !cleaned.is_empty() {
                                    return Some(cleaned);
                                }
                            }
                        }
                    }
                }
            }
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

/// Fetch remote txt file content via HTTP/HTTPS
pub async fn fetch_http_txt(url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }

    resp.text().await.ok()
}

/// Resolve server configuration from user input string:
/// 1. "http://..." or "https://..." -> Fetch URL content
/// 2. "txt:domain.com" -> Query TXT record explicitly
/// 3. "domain.com" (without port) -> Query TXT record; if present, use it; otherwise fallback to input
pub async fn resolve_server_config(input: &str) -> Option<ResolvedServerConfig> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        if let Some(content) = fetch_http_txt(input).await {
            return parse_txt_content(&content);
        }
        return None;
    }

    if let Some(domain) = input.strip_prefix("txt:") {
        if let Some(txt) = query_dns_txt(domain).await {
            return parse_txt_content(&txt);
        }
        return None;
    }

    if !input.contains(':') && input.contains('.') {
        if let Some(txt) = query_dns_txt(input).await {
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

    #[test]
    fn test_parse_simple_txt() {
        let content = "\"rustdesk.6143443.xyz:22111\"";
        let res = parse_txt_content(content).expect("failed to parse");
        assert_eq!(res.host, "rustdesk.6143443.xyz:22111");
        assert_eq!(res.relay, None);
    }

    #[test]
    fn test_parse_kv_txt() {
        let content = "host=1.2.3.4:22111,relay=1.2.3.4:22112,key=mysecretkey";
        let res = parse_txt_content(content).expect("failed to parse");
        assert_eq!(res.host, "1.2.3.4:22111");
        assert_eq!(res.relay, Some("1.2.3.4:22112".to_string()));
        assert_eq!(res.key, Some("mysecretkey".to_string()));
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
