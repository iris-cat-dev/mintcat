# mintcat-proxy 技术说明

本文档描述 `mintcat-proxy` 的架构、数据流与各模块职责，便于维护与二次开发。

## 1. 设计目标

在最小依赖下复现 Watt Toolkit 加速管线的核心路径：

1. **规则拉取**：从 Watt 公开 API 获取 `AccelerateProject` 树形结构，并落盘为 JSON 缓存。
2. **流量接入**：通过 **Hosts** 将 `ListenDomainNames` 指向本机（默认 `127.0.0.1`，当 `bind` 为 `0.0.0.0` 时）。
3. **TLS 终结**：客户端以 HTTPS 访问被劫持域名；本地使用 **自签 CA + 按 SNI 动态签发叶子证书** 完成 TLS 握手。
4. **上游转发**：解密后的 HTTP 请求按规则中的 **ProxyType** 与 **ForwardDomainNames** 等字段，构造目标地址并转发；**不**使用系统默认 DNS 解析上游，而使用 **DoH**，避免与 Hosts 冲突。

## 2. 总体架构

```
┌─────────────────┐     HTTPS      ┌──────────────────┐
│ Steam / 浏览器   │ ────────────► │ mintcat-proxy    │
│ (SNI=真实域名)   │   127.0.0.1  │ (rustls + hyper) │
└─────────────────┘  :443          └────────┬─────────┘
                                            │
                     ┌──────────────────────┼──────────────────────┐
                     │                      │                      │
                     ▼                      ▼                      ▼
              ┌─────────────┐      ┌──────────────┐      ┌──────────────┐
              │ Watt API    │      │  hosts.rs    │      │  dns.rs      │
              │ config.rs   │      │  标记区写入   │      │  DoH 解析    │
              └─────────────┘      └──────────────┘      └──────────────┘
                     │                      │                      │
                     └──────────────────────┴──────────────────────┘
                                            │
                                            ▼
                                    ┌──────────────┐
                                    │  上游真实站点  │
                                    │  (IP/HTTPS)   │
                                    └──────────────┘
```

## 3. 模块与源文件

| 模块 | 文件 | 职责 |
|------|------|------|
| 服务与租约 | `main.rs` / `lib.rs` / `protocol.rs` | daemon 持有已签名 artifact 的 ModuleLease 与 Child；JSON-lines ready/health/stop 校验 artifact/version/protocol/instance；控制管道关闭时退出 |
| 规则与 API | `config.rs` | `WattApiClient::fetch_all` → `GET https://api.steampp.net/api/Accelerate/All`；序列化缓存；`flatten_projects` 将 `AccelerateProjectGroup` 展平为 `DomainConfig` |
| 证书 | `cert.rs` | `rcgen` 生成/加载 CA；`CertManager` 实现 `rustls::server::ResolvesServerCert`，按 SNI 缓存叶子证书；跨平台安装/卸载根信任 |
| 代理 | `proxy.rs` | `hyper` 服务侧：根据 Host 匹配 `DomainConfig`，`reqwest` 转发到上游 URL |
| DNS | `dns.rs` | `DohResolver`：JSON DoH（RFC 8484 的 `application/dns-json` 变体），多 provider 失败回退 |
| Hosts | `hosts.rs` | 在系统 hosts 中插入/删除 `# mintcat-proxy Start` … `End` 标记块 |

> 计划中曾单独列出 `tls.rs`；当前实现将 **TLS 证书解析** 合入 `cert.rs`（`ResolvesServerCert` trait）。

## 4. 加速规则与转发语义

规则来自云端 `AccelerateProjectDTO` 等价结构（JSON 字段 `camelCase`）。本地结构为 `DomainConfig`，由 `collect_domain_configs` 递归展开带子项的节点。

### 4.1 ProxyType 与上游

与 Watt Toolkit 中 `AccelerateProjectDTO` 实现 `IDomainConfig` 的语义对齐：

| ProxyType | ForwardDomainNames / Port | 本地行为 |
|-----------|---------------------------|----------|
| **Local** | 可解析为 IP | `forward_ip`：直连该 IP，URL 为 `https://{ip}{path}`，`Host` 头保持原始域名 |
| **Local** | 非 IP 字符串 | `forward_domain`：先 DoH 解析该域名得到 IP，再同上 |
| **Redirect** | 主机名 + `port` | `destination_url`：`{http\|https}://{host}:{port}/` |
| **ServerAccelerate** | 完整 URL | `destination_url` + `is_server_proxy`：请求发往该 URL；可扩展 `X-Watt-*` 头（与官方一致时需 token，当前实现以简化为主） |

未匹配到规则时：**DoH 解析请求 Host 中的域名**，再直连解析到的 IP（纯 DNS 防污染兜底）。

### 4.2 证书与 TLS

- **CA**：独立 durable action 批准 `create-credentials` 后，受限 broker 在 daemon-owned 数据目录创建 `ca.pem` / `ca.key`（Unix 0600 私钥，0700 目录）。start 不隐式创建或安装证书。
- **叶子证书**：对每个 SNI 域名生成 ECDSA P-256 叶子证书，由 CA 签名，私钥仅用于 rustls。
- **信任**：安装/移除根信任各需独立系统批准；broker 只运行固定平台证书工具，不接受 shell、可执行文件或调用者文件路径。

### 4.3 DNS 与 Hosts 的交互

Hosts 将域名指向本机后，若上游仍用 **系统解析器** 解析同一域名，会再次得到 `127.0.0.1`，形成死循环。因此 **DoH 解析器不依赖系统 DNS**，且直连 IP 时使用 IP 字面量作为连接目标。

## 5. API 契约（简要）

- **Base URL**：`https://api.steampp.net`
- **列表接口**：`GET /api/Accelerate/All`
- **响应**：外层包含 `content` 字段，类型为 `AccelerateProjectGroup[]`（具体字段名以实际 JSON 为准）。

若 API 变更或需鉴权，需同步更新 `config.rs` 中的 `ApiResponse` 与 `Deserialize` 结构。

## 6. 安全与运维注意

1. **根密钥**：`ca.key` 泄露可导致任意域名被伪造，请限制目录权限。
2. **MITM 范围**：仅用于被 Hosts 劫持、且客户端信任该 CA 的流量。
3. **443 占用**：daemon 默认 unprivileged 8443；真实 Steam hosts 加速仍使用 443，需独立 privileged listen action 及系统授权。bind 失败会清理并 reap owned child，不终止占用端口的其他进程。
4. **服务端加速**：完整复现 Watt 官方「服务端代理」需账号 Token 等，本 CLI 为简化实现。

## 7. 依赖栈

| 依赖 | 用途 |
|------|------|
| tokio | 异步运行时 |
| rustls / tokio-rustls | TLS 服务端 |
| rcgen | X.509 与 CA/叶子证书生成 |
| hyper / hyper-util | HTTP/1.1–2 服务 |
| reqwest | 上游 HTTP(S) 客户端 |
| clap | CLI |
| serde / serde_json | 配置与 API 反序列化 |
| dashmap | 证书缓存 |
| tracing | 日志 |

## 8. 扩展与改进方向

- [ ] 实现 `FakeServerName` / `IgnoreSSLCertVerification` 与上游 TLS 的完整对齐（SNI 伪装、证书校验策略）
- [ ] `ServerAccelerate` 与 `GenerateServerSideProxyToken` 等官方流程对接
- [ ] 独立 `tls.rs` 或单元测试覆盖证书与 DNS 解析
- [ ] HTTP/3（QUIC）上游（视需求）

---

*文档版本与 `Cargo.toml` 中 `version` 同步维护为宜。*
