# RustDesk 动态 DNS TXT 解析适配与 STUN (TCP/UDP 分离) 开发设计文档

## 1. 概述与背景

### 1.1 业务背景
在家庭宽带或无固定公网 IPv4 的内网穿透自建 RustDesk 场景中，通常借助 **Lucky 等 NAT1 STUN 打洞工具** 将内网中的 `hbbs` (ID/注册信令服务) 与 `hbbr` (中继服务) 暴露至公网。

由于动态 NAT 打洞的公网 IP 和端口会随运营商重拨号或隧道刷新而变动，通过 **DNS TXT 记录（由 Lucky WebHook 动态维护更新）** 分发最新的服务器公网连接信息是最佳的轻量化免运维方案。

### 1.2 核心技术难点与痛点
1. **DNS-over-HTTPS (DoH) 国内网络握手异常**：原先纯依靠 DoH（443 端口）在部分客户端网络容易出现 TLS Handshake RST 或超时。
2. **端口覆盖与类型转换 Bug**：官方客户端在处理外部输入域名时默认强行追加 `:21116`，导致正则/冒号提取失败或在 `get_rendezvous_server` 列表过滤中回退至默认端口。
3. **STUN 单协议打洞限制（TCP / UDP 分离矛盾）**：
   - Lucky 的 STUN 穿透规则基于单协议打洞（1 条规则 = 1 个协议）。
   - `hbbs` (21116) 的 UDP 端口用于被控端在线心跳与 P2P 打洞（映射为公网端口 A）；
   - `hbbs` (21116) 的 TCP 端口用于主控端远程连接信令 `PunchHoleRequest`（映射为公网端口 B）；
   - `hbbr` (21117) 的 TCP 端口用于中继流量传输（映射为公网端口 C）；
   - 官方 RustDesk 默认假设 `hbbs` 的 TCP 与 UDP 共享同一端口号，无法适配 STUN 生成的不同端口。

---

## 2. 系统架构与通信流图

```mermaid
flowchart TD
    subgraph Lucky_NAT_Mapping["Lucky STUN 动态端口映射"]
        hbbs_udp["hbbs (内网 21116 UDP)"] -->|STUN 规则 1 (UDP)| Ext_UDP["公网 host (例: 198.51.100.123:24869)"]
        hbbs_tcp["hbbs (内网 21116 TCP)"] -->|STUN 规则 2 (TCP)| Ext_TCP["公网 tcp (例: 198.51.100.123:24439)"]
        hbbr_tcp["hbbr (内网 21117 TCP)"] -->|STUN 规则 3 (TCP)| Ext_RELAY["公网 relay (例: 198.51.100.123:24867)"]
    end

    subgraph DNS_System["DNS TXT 记录分发"]
        Lucky_NAT_Mapping -->|Lucky WebHook 自动推送到 DNS| DNS_Record["TXT: host=24869,tcp=24439,relay=24867,key=..."]
    end

    subgraph RustDesk_Client["RustDesk 客户端适配层 (txt_resolver.rs)"]
        DNS_Record -->|UDP 53 极速查询 / DoH 备用| Resolver["TXT 解析引擎 (libs/base/src/txt_resolver.rs)"]
        Resolver --> ResolvedCfg["ResolvedServerConfig { host, tcp, relay, key }"]
    end

    subgraph Runtime_Behavior["业务运行与自动分流"]
        ResolvedCfg -->|UDP 注册 & 心跳| Controlled["被控端服务 (rendezvous_mediator.rs)"]
        ResolvedCfg -->|TCP 信令交互 (PunchHoleRequest)| Controlling_TCP["主控端信令连接 (client.rs)"]
        ResolvedCfg -->|UDP 打洞探测| Controlling_UDP["主控端 NAT 打洞套接字 (client.rs)"]
        ResolvedCfg -->|TCP 中继数据转发| Relay_Stream["中继流传输 (client.rs -> hbbr)"]
        
        Controlled --> Ext_UDP
        Controlling_UDP --> Ext_UDP
        Controlling_TCP --> Ext_TCP
        Relay_Stream --> Ext_RELAY
    end
```

---

## 3. TXT 记录规范与解析规则

### 3.1 TXT 记录格式定义

| 键名 | 是否必需 | 说明 | 示例 |
| :--- | :---: | :--- | :--- |
| `host` | 是 | `hbbs` 的公网地址（在 STUN 模式下对应 **UDP 端口**） | `198.51.100.123:24869` |
| `tcp` | 否 | `hbbs` 的公网信令 **TCP 端口**（未配置时自动回退为 `host`） | `198.51.100.123:24439` |
| `relay` | 否 | `hbbr` 的公网中继 **TCP 端口** | `198.51.100.123:24867` |
| `api` | 否 | Web/Api 服务地址（解析后自动写入 `api-server` 选项） | `http://198.51.100.123:21114` |
| `online` | 否 | 在线状态查询端口（解析后自动写入 `online-server` 选项） | `198.51.100.123:24438` |
| `key` | 已废弃 | 服务端强制验证公钥（`-k _` 生成的 `.pub`）。**客户端可解析但不再采用**：TXT 是无签名通道，采用 TXT 下发的 key 会使 DNS 欺骗可直接注入伪造公钥实施中间人。公钥请在客户端"安全/Key"或 profile 的 key 字段**手动填写**（手动配置后不会再被任何解析覆盖） | — |

#### 标准 Lucky STUN 穿透 TXT 示例：
```text
host=198.51.100.123:24869,tcp=198.51.100.123:24439,relay=198.51.100.123:24867,online=198.51.100.123:24438,api=https://rustdesk-api.yourdomain.com
```

#### 标准官方自建服务器（同一端口）TXT 示例：
```text
host=rd.yourdomain.com:21116,relay=rd.yourdomain.com:21117
```

---

## 4. 核心实现与代码修改解析

### 4.1 `libs/base/src/txt_resolver.rs`：原生 UDP DNS 查询与字段解析

#### 1. 解析数据结构扩展
```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedServerConfig {
    pub host: String,        // UDP / 默认服务器地址
    pub tcp: Option<String>, // TCP 信令独立端口 (STUN 场景专用)
    pub relay: Option<String>,
    pub api: Option<String>,
    pub key: Option<String>,   // 仅为兼容旧 TXT 格式保留解析，调用方一律不采用
    pub online: Option<String>,
}
```

#### 2. 原生 UDP 53 端口 DNS 解析与 DoH 回退
实现了 `query_dns_txt_udp`，构造原生标准 DNS 查询报文（事务 ID 每次随机），**并发**向公共 DNS 服务器（`223.5.5.5`、`119.29.29.29`、`180.76.76.76`、`1.1.1.1`、`8.8.8.8`）发送、先到先得，通常数百毫秒内即可完成解析，规避 DoH TLS 握手 reset；全部 UDP 失败时再并发回退 4 个 DoH 端点。响应必须通过事务 ID 回显、RCODE、问题区域名与答案 owner name 校验，防止把 SPF/DKIM 等无关 TXT 记录或伪造响应误当作服务器配置。官方服务器域名（`*.rustdesk.com` 及内置列表）无条件跳过 TXT 解析。

#### 3. 结果缓存
`resolve_server_config` 带 30 秒 TTL 的进程内缓存（含负缓存），热点调用路径（`get_rendezvous_server`、被控端 60 秒周期检查）不会重复发起网络查询，也不阻塞心跳主循环。

#### 4. 兼容带端口输入的域名剥离
```rust
pub async fn resolve_server_config(input: &str) -> Option<ResolvedServerConfig> {
    ...
    let host = input.split(':').next().unwrap_or(input).trim();
    // 如果是域名且非原生 IP，均自动查询 TXT 记录
    if !host.is_empty() && host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        if let Some(txt) = query_dns_txt(host).await {
            if let Some(cfg) = parse_txt_content(&txt) {
                return Some(cfg);
            }
        }
    }
    None
}
```

---

### 4.2 `src/common.rs`：锁定动态解析端口与 TCP 映射配置

#### 1. 防止覆盖动态端口 (`resolved_from_txt`)
在 `get_rendezvous_server` 中，检测到 TXT 动态解析成功后，更新本地 `rendezvous-server-tcp`、`relay-server`、`api-server` 与 `online-server` 运行时配置，并从备选列表中剔除原始静态域名，防止 `b.pop()` 覆盖动态端口。**TXT 中的 `key` 一律不写入任何配置**，验证公钥只能由用户手动设置（手动值不会被解析覆盖）；多 profile 模式下 `tcp`/`relay` 走 per-profile 数据隔离，`api`/`online` 始终按当前活动服务器刷新全局选项。

```rust
    if let Some(resolved) = txt_resolver::resolve_server_config(&a).await {
        a = resolved.host;
        resolved_from_txt = true;
        if let Some(api) = resolved.api {
            Config::set_option("api-server".to_owned(), api);
        }
        if let Some(online) = resolved.online {
            Config::set_option("online-server".to_owned(), online);
        }
        if !has_multi {
            if let Some(tcp) = resolved.tcp {
                Config::set_option("rendezvous-server-tcp".to_owned(), tcp);
            }
            if let Some(relay) = resolved.relay {
                Config::set_option("relay-server".to_owned(), relay);
            }
        }
    }
```

#### 2. NAT 探测与 TCP 代理连接切流
```rust
    let (server1, _, _) = crate::get_rendezvous_server(1_000).await;
    let tcp_opt = Config::get_option("rendezvous-server-tcp");
    let server1 = if !tcp_opt.is_empty() { tcp_opt } else { server1 };
    let server2 = crate::increase_port(&server1, -1);
```

---

### 4.3 `src/client.rs`：主控端 TCP 信令与 UDP 打洞分流

在主控端建立远程桌面连接时：
- **TCP 信令连接（`PunchHoleRequest`、中继协调）**：优先连接 `rendezvous-server-tcp`（如 `24439`）；
- **UDP 打洞（`new_direct_udp_for`）**：保持连接 `host`（如 `24869`）。

```rust
    let tcp_opt = Config::get_option("rendezvous-server-tcp");
    let orig_udp_server = rendezvous_server.clone();
    if !tcp_opt.is_empty() {
        rendezvous_server = tcp_opt;
    }
    let mut start = Instant::now();
    let mut socket = connect_tcp(&*rendezvous_server, CONNECT_TIMEOUT).await;
    log::info!("rendezvous server (tcp): {}, (udp): {}", rendezvous_server, orig_udp_server);
```

---

### 4.4 `src/rendezvous_mediator.rs`：被控端心跳注册与动态热重载

被控端服务在启动与定时检查（`last_dns_check`）过程中，自动保持对 TXT 记录的跟踪：
- 被控端注册与保活 UDP 数据包发送至 `host`（`24869`）；
- 当检测到 TXT 中的 IP 或端口发生变更时，触发热重载自动重新建立连接。

---

## 5. Lucky STUN 隧道与 WebHook 配置实操指南

### 5.1 Lucky 管理端配置（3 条 STUN 规则）
在 Lucky 的【STUN内网穿透】模块下建立 3 条规则：

1. **规则 1：hbbs UDP 穿透**
   - 监听/目标 IP: `127.0.0.1`（或 Docker 内部容器 IP）
   - 目标端口: `21116`
   - 穿透协议类型: **UDP**
2. **规则 2：hbbs TCP 穿透**
   - 监听/目标 IP: `127.0.0.1`
   - 目标端口: `21116`
   - 穿透协议类型: **TCP**
3. **规则 3：hbbr TCP 穿透**
   - 监听/目标 IP: `127.0.0.1`
   - 目标端口: `21117`
   - 穿透协议类型: **TCP**

### 5.2 Lucky WebHook 动态推送到 DNS TXT
在 Lucky 的 WebHook 回调配置中，将各条 STUN 规则获取到的外部 IP 和端口拼接为如下内容并更新到域名 TXT 记录：
```text
host=#{规则1公网IP}:#{规则1公网端口},tcp=#{规则2公网IP}:#{规则2公网端口},relay=#{规则3公网IP}:#{规则3公网端口}
```
> 旧格式中的 `,key=...` 字段可以保留（不影响解析），但客户端已不再采用 TXT 下发的 key，公钥需在客户端手动配置一次。

---

## 6. 兼容性说明

1. **官方自建服务（单端口 TCP+UDP 模式）**：
   - 若 TXT 记录中未声明 `tcp=`（如仅有 `host=rd.domain.com:21116`），系统自动将 TCP 信令回退到与 `host` 相同的端口，完全保持与官方行为一致。
2. **直连 IP/域名连接**：
   - 若用户直接输入 `1.2.3.4:21116` 或 `rd.domain.com:21116`，代码能够正常直连，互不影响。
3. **验证 Key**：
   - 服务器开启 `-k _` 强制验证时，公钥需在客户端"安全/Key"（或 profile 的 key 字段）手动填写一次；TXT 通道不再分发 key，填写后不会被任何动态解析覆盖。官方服务器与无验证（`-k n` 或默认）场景无需任何操作。
4. **无关 TXT 记录**：
   - 域名上存在的 SPF/DKIM/站点验证等 TXT 记录会被格式校验与 owner 校验拒绝，不会被误认作服务器地址；官方 RustDesk 域名完全跳过 TXT 解析。
