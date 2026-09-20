# PR 正文（中文版）— 目标：lejianwen/rustdesk-server 分支 `forapi`

标题：

```
feat(rendezvous): enable UDP + IPv6 hole punching in open-source hbbs
```

---

### 问题

在「开源 hbbs（本仓库 forapi 分支）+ 原版客户端（PC 1.5.x / 官方安卓 1.4+）」的组合下，UDP 打洞与 IPv6 直连永远不会生效——所有跨 NAT 会话只能走中继。客户端侧逻辑本来就是齐的，缺的只是服务端，共三处：

1. **UDP 的 `TestNatRequest` 被直接丢弃。** `handle_udp` 没有对应分支，客户端永远拿不到自己的 UDP 映射端口，`PunchHoleRequest.udp_port` 恒为 0，`Client::start` 里的 UDP 打洞路径根本不会启动。
2. **`udp_port` / `is_udp` 从不透传。** `PunchHole` 不带 A 的 UDP 观测端口给 B；B 经 UDP 送达的 `PunchHoleSent` 构造 `PunchHoleResponse` 时也不置 `is_udp`。更糟的是此时响应被从 UDP 发往 A 的 **TCP** 观测地址——纯死信。
3. **`socket_addr_v6` 从不填充。** 客户端 IPv6 分支只认 `PunchHole.socket_addr_v6` 这个专用字段（`socket_addr` 里塞 v6 地址不算数），并且**精确拨向该端口**。开源 hbbs 从不填它，所以 IPv6 介绍实际是 Pro 服务端私有功能。注意这里必须是 A **自报**的 IPv6 UDP 打洞 socket（`PunchHoleRequest.socket_addr_v6`）：我们在 TCP 上观测到的 A 源端口是一次性连接端口，UDP 上无人监听，拿它回拨永远打不通。

### 改动（两个提交，仅 `src/rendezvous_server.rs`，+53/−2）

- `feat(rendezvous): enable UDP hole punching in open-source hbbs` —— `handle_udp` 新增 TestNat 应答（回观测源端口，附带 ConfigUpdate），与 TCP 分支对齐；`PunchHole` 转发 `udp_port`；`PunchHoleResponse` 置 `is_udp` 并同时经 UDP 与 A 的 TCP 打洞连接双通道投递。
- `feat(rendezvous): pass through socket_addr_v6 to enable IPv6 hole punching` —— `PunchHole.socket_addr_v6` 透传 A 的自报值（A 走 v6 但未自报时回退观测地址）；`PunchHoleResponse.socket_addr_v6` 透传 B 的 `PunchHoleSent.socket_addr_v6`（同样回退规则）。

纯协议字段填充，不改 proto、不改 API；对 IPv4/TCP 用户零行为变化，客户端本就具备消费这些字段的逻辑。

### 实测验证（2026-09-20，真实部署）

环境：家里 Windows PC（NAT 后，有 GUA）+ 小米手机 5G（官方 APK，手机自测 UDP NAT 为 SYMMETRIC），patched hbbs 部署在 NAS 节点与云主机两处。

- 补丁前：`udp_nat_port` 恒为 0，所有远程会话走中继。
- UDP TestNat 回环探测：原版 hbbs 对 UDP 请求完全静默，patched 版返回 `TestNatResponse{port}`。
- 补丁后：5G 手机 ↔ PC **IPv6 直连建立仅 85–102 ms**（KCP connect 日志），中继回退路径不再触发。

欢迎测试反馈，随时配合复测。
