# Agent CLI 架构与迁移计划

## 问题

MintCat 的 Agent 工具能力已经集中在 `AgentToolRegistry` 和 `ModAgentToolService`，但执行链仍依赖前端运行时：

- SQLite 通过 `@tauri-apps/plugin-sql` 访问；
- 在线搜索依赖现有 TypeScript provider；
- 添加、下载、启停和移动 Mod 复用 `HomeService`、`OnlineModAddService`；
- Nexus Mods 下载授权需要 MintCat 管理的 WebView。

直接在 Rust CLI 中重新实现这些流程会形成第二套业务规则，并带来数据库写入、下载状态、代理、认证和 UI 刷新不一致的问题。

## 当前决策

先稳定外部契约，不复制业务实现：

```text
external AI agent
        │ JSON/stdout
        ▼
   mintcat-cli
        │ authenticated loopback JSON protocol
        ▼
Rust Control Plane bridge
        │ typed Tauri event
        ▼
AgentCliService (TypeScript)
        │ shared AgentToolExecutor contract
        ▼
ModAgentToolService / existing services
```

`mintcat-cli` 是独立、轻量的 Rust 可执行文件。MintCat 桌面端在随机 loopback 端口监听，并在用户配置目录写入 `agent-cli.json`。该文件包含协议版本、PID、地址和 256-bit 随机 token；Unix 权限为 `0600`。CLI 只连接 loopback，不接受远端地址。

此阶段要求 MintCat 桌面端正在运行且核心初始化完成。该限制是显式的：CLI 的 `status` 返回 `ready`，未运行或未就绪时返回结构化错误，不会绕过现有业务服务直接写数据库。

## 命令契约

```bash
mintcat-cli status
mintcat-cli tools
mintcat-cli call <tool> [--input '<json>']
mintcat-cli call <tool> --input-file <path|->
mintcat-cli confirm <action-id>
mintcat-cli reject <action-id>
```

全局参数：

- `--pretty`：格式化 JSON；默认输出单行 JSON，便于 agent 解析。
- `--endpoint <path>`：覆盖 endpoint 文件路径。
- `MINTCAT_CLI_ENDPOINT`：与 `--endpoint` 等价的环境变量。

所有正常命令只向 stdout 写一个 JSON 对象：

```json
{
  "protocolVersion": 1,
  "requestId": "opaque-request-id",
  "ok": true,
  "data": {}
}
```

错误保持同一 envelope：

```json
{
  "protocolVersion": 1,
  "requestId": "opaque-request-id",
  "ok": false,
  "error": {
    "code": "invalid_arguments",
    "message": "limit must be an integer from 1 to 100"
  }
}
```

退出码：

| Code | 含义 |
| --- | --- |
| `0` | 命令成功；变更命令已执行或已成功生成待确认 action |
| `1` | MintCat 返回工具或业务错误 |
| `2` | CLI 参数或输入 JSON 无效 |
| `3` | endpoint、协议或本地 bridge 不可用 |

`tools` 返回 OpenAI-compatible function schemas，以及 read-only/mutation 分类和确认策略。它是外部 agent 的能力发现入口，不应在 agent 侧复制工具清单。

## 变更安全

读取工具立即执行。变更工具采用两阶段协议：

1. `call` 只校验并生成 action，返回 `confirmationRequired: true`、标题、影响详情和过期时间；
2. 外部 agent 获得用户授权后调用 `confirm <action-id>`；也可调用 `reject`；
3. action 为一次性、五分钟有效；重复确认、过期确认和 Profile 切换都会失败；
4. CLI 没有 `--yes`，避免模型在同一次工具调用中绕过确认边界。

当前 token 保护的是同一用户会话中的本地传输。能读取用户配置目录的同用户进程本来就能读取 MintCat 数据库，因此不建立更高的本地权限边界。

## 后续重构顺序

协议 v1 保持不变，执行端可逐步从 TypeScript 迁移到 Control Plane：

1. **应用端口稳定化**：继续从 UI/ViewModel 中移出业务逻辑，所有 Agent 与 UI 调用共享 `AgentToolExecutor` 用例接口。
2. **只读能力迁移**：将 active profile、folder、mod 查询移入 Rust application core；用同一组 CLI contract fixtures 验证结果字段和过滤语义。
3. **本地变更迁移**：迁移 rename、enable、version、move、remove，并由 Rust 统一事务、事件和 Profile 并发校验。
4. **在线能力迁移**：最后迁移 provider、代理、认证和下载状态机；Nexus WebView 保留为可注入授权 adapter。
5. **Headless 模式**：所有命名能力迁移完成后，CLI 可直接加载 Rust application core，不再要求桌面端运行。切换只替换 bridge 后端，不改变命令、JSON envelope 或工具 schema。

每一步采用完整 cutover：迁移一个工具时同时迁移 UI 和 Agent 调用方，删除旧实现，不长期维护双写或 fallback。
