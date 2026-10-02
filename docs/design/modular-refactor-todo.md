# Mintcat 大规模重构：后续 Agent 执行 TODO

状态：**实现与验收进行中**。当前 29 项已验收，R25 按用户要求取消继续接入，6 项仍缺完整验收前提。代码落盘、子任务交接和历史测试通过都不等于当前整合版本可交付；最新进度见 [当前交接点](#e-当前交接点)。

## 先读什么

1. [总体架构设计](modular-runtime-architecture.md)：进程/模块边界、协议、数据模型、UI、游戏打包安装、更新与迁移。
2. [下载与原子能力锁专项](mod-download-concurrency.md)：必须落实的并发协议，不是可选优化。
3. 本文件：按前置依赖执行、记录实际证据和当前执行位置。

总体设计第 11.8 节规定**各游戏插件独立发布、更新、依赖绑定、灰度与回滚**。不得改成一个重新打包全部游戏的总 DLL，也不得以一个全局 engine activeVersion 重新耦合游戏。

## A. 执行规则与完成定义

### A.1 工作区和数据保护

- 当前工作区存在用户未提交的 CLI、Agent、打包配置等修改。开始时读取当前状态和相关 diff，保留用户变更，不 reset/checkout/clean 覆盖。
- `apps/`、`runtime/`、`modules/`、`packages/` 已有实际实现；旧 `src/` 与 `src-tauri/` 的执行入口正在切除。先定位当前消费者再修改，不假设旧入口仍可用；保留仍被游戏模块使用的纯算法与用户无关文件。
- R00–R33 在隔离的 vNext data-dir/测试目录进行；不能让新 Library 和旧前端同时写同一真实数据库。
- 不接触真实游戏目录、全局证书、生产发布指针或真实用户凭据，除非对应验证已具备明确授权和可恢复条件。
- 原根目录 `TODO` 的三条事项保留：RC 清理缺失 Mod 异常、每游戏独立打包设置、游戏启动检查 bug。R20/R21 已记录针对性 fixture 回归，但当前完整切换与授权真机验收尚未完成，不能将原事项无条件划掉。

### A.2 如何认领、执行和交接

1. 选择前置依赖全部完成且未被其他 Agent 占用的最早任务。
2. 阅读该卡片和引用设计，再查看当前实现；文件可能已被前一个 Agent 迁移，不能盲用历史行号。
3. 确定该卡片的单一集成负责人。并行 Agent 只能分配明确、不冲突的切片；共享协议/数据库结构先由负责人定好。
4. 完成全部动作、调用者迁移和真实路径验证，再勾选任务。
5. 在卡片下追加执行记录：实际修改文件、运行命令/场景、观察结果、明确限制。未执行的验证不能写“通过”。
6. 卡住时标记 blocked 和精确缺失项，记录已尝试；可执行的独立任务继续推进。不能把 blocked/stub 当 done。
7. 下一个 Agent 从未完成且依赖满足的任务继续，不重复设计已明确的基本架构。

每个任务的执行记录格式：

```text
执行状态：未开始 / 进行中 / blocked / 已完成 / 已取消（非验收通过）
负责人：实际执行者
实际产物：文件或接口清单
验证证据：命令/场景 -> 观察结果
限制/阻塞：明确事实；无则写无
```

以上是记录格式，不要求现在伪造负责人或验证结果。

### A.3 每个复选框的统一 Done 门槛

必须同时满足：

- 卡片中的消费者可见行为实现完整，不只是 crate、interface、注册表或占位 handler 存在。
- 所有该用例 GUI/CLI/Agent 调用方切到同一权威实现；没有失败后退回旧前端的 fallback。
- 权限、目标身份、revision、幂等、错误与取消遵循已定义契约。
- 实际运行改变过的路径；UI 看实际界面，CLI 执行真实程序，文件事务操作临时目录并检查最终内容。
- 必要的行为/边界回归测试通过；不新增验证 import、文案、复制、mock echo 或内部实现形状的测试。
- 删除此 cutover 已经替代的旧实现/事件/别名；更新受影响文档，用户无关修改保持不变。
- 最终执行记录足够让后续 Agent 复核，不依赖聊天上下文。

### A.4 不允许悄悄改变的决定

- Rust/Tauri/React 保留；业务迁出 renderer，CLI 不直接写库。
- 官方签名 native worker；进程隔离不冒充安全沙箱。
- Library 单写者，Profile 内容门禁与业务写入在同库短事务内。
- prepare-and-apply 下载锁当前 Profile 的内容编辑；cache-only 不改 Profile；迟到结果必须通过 fence/revision/incarnation 校验。
- 游戏构建与文件提交分开，不先卸载再尝试构建。
- 每个游戏独立 ScopeReleaseSet；共享无状态依赖可多版本并存；公共有状态单例不允许不兼容双写。
- 旧任务固定原模块/资源 digest；下载更新不能覆盖运行中文件。
- schema 不兼容时不能静默恢复旧备份丢掉新写入。

## B. 依赖和并行路线

按任务编号推进；同组满足依赖的任务可以并行。不得为追求并行让多个 Agent 同时改 contracts/Library schema。

```text
R00 -> R01 -> R02 -> R03
                    R03 -> R04, R05, R06
R06 + R05 -> R07 -> R08
R04 + R03 -> R09 -> R10 / R11 / R12
R04 + R01 -> R13
R04 + R05 + R09 -> R14
R07 + R08 + R10..R14 -> R15 -> R16
R04 + R05 -> R17
R04 + R13 -> R18 -> R19
R18 + R19 -> R20 / R21
R08 + R15 + R17 + R20 + R21 -> R22
R16 + R22 -> R23 -> R24
R06 + R07 + R09 -> R26
R04 -> R27
R27 + R05 + R07 + R17 -> R28
R23 + R28 -> R29
R20 + R21 + R22 + R28 + R29 -> R30
R27 + R04 + R23 -> R31
R08 + R15 + R22 + R24 + R26 + R28 -> R32
R20 + R21 + R27 + R28 + R31 + R32 -> R33
R23 + R24 + R26 + R29 + R30 + R33 -> R34 -> R35
```

说明：R10..R14 表示 R10、R11、R12、R13、R14 全部。依赖卡片是代码实现依赖，不表示真实线上发布授权已经存在。

R25 已取消继续产品接入，不作为 R32–R35 的前置；保留既有实验代码和历史普通服务证据，不把取消记为系统验收通过。

## C. 任务卡片

### 第一组：运行时、协议和单一数据所有者

- [x] **R00 — 固定现状边界、保留资产与验证输入**
  - 前置：无。
  - 读取：三份设计/任务文档；`package.json`、`src-tauri/Cargo.toml`、现有 CLI/更新/安装入口及工作区变更。
  - 动作：建立当前 GUI/CLI/Agent 用例清单；确认操作语义、游戏支持、发布入口、外部 Proxy/发布服务位置；整理合法小型 PAK/ZIP/目录与旧数据库副本。
  - 产物：在本卡执行记录中给出“能力 -> 当前实现 -> 目标 owner -> 验证输入”映射和确实缺失的外部条件，不另造一套竞争性总体方案。
  - 验收：覆盖列表/分组/排序/版本/来源/认证/安装/打包/卸载/Agent/Proxy/备份/更新及 standard/portable/fixed-WebView 三条发行路径。
  - 禁止：为了确认用户已经报告的问题而重跑破坏性操作；把代码文档描述当成已通过的运行证据。

  执行状态：已完成

  负责人：omp coding assistant

  当前边界与操作语义：

  - 当前支持 `drg` 与 `rc` 两个游戏。游戏、来源和安装路径仍由 `DatabaseInitializer`、`IntegrateApi`、`ModSearchService`、`ModMapper` 及 integrator `GameKind` 分支硬编码；未知 PAK 路径会被 integrator-api 默认为 DRG。
  - Profile/列表查询会经 `ProfileService.ensureActiveProfile()` 隐式创建或激活 Profile；Agent 读取也会触发该路径。显示名是跨 Profile 共享字段，删除默认只删 Profile 关联；排序写回会先删关联/文件夹再重建整棵树。
  - `usedVersion`、来源的 latest、缓存文件和安装版本尚未分离。`ModUpdateService.updateModFile` 在网络前后分段写库，进度按 `modId` 携带整份旧 `CompleteModData`；没有 revision、incarnation、lease 或 fence。
  - 安装由 renderer `ModInstallTask` 读取 active Profile，下载后先卸载旧版本，再经进程内 `libloading` DLL 写游戏目录；完成依赖无 jobId 的全局事件。当前没有独立的“只构建/打包到 output grant”产品用例或文件 receipt。
  - 当前 Agent CLI 是用户未提交的工作：`status/tools/call/confirm/reject` -> loopback TCP bridge -> renderer `AgentCliService`；需要 GUI ready，action/search result 只在 JS `Map`，调用方断线不取消实际操作。本卡未改写这些文件。
  - 自定义 frontend/integrator/proxy 更新各有 active 状态。frontend/integrator 的 `PUBLIC_KEY` 为空，proxy 只检查签名非空；`hot_update.rs` 的 release set/safe mode 状态没有控制 integrator loader。

  | 能力 | 当前实现/入口 | 目标 owner | 已固定验证输入 |
  | --- | --- | --- | --- |
  | Profile、列表与查询 | `ProfileService`、`HomeService`、`ProfileDAO`/`ModDAO`，renderer 直连 SQLite | `mintcat.library` | `legacy/mintcat-v0.6.sqlite` 中 DRG/RC Profile、共享 Mod、顺序与启停 |
  | 分组、移动与排序 | `ProfileTreeService` 整树重建、`ProfileDAO` | `mintcat.library` | 旧库的根/嵌套分组及稳定 `sort_order` |
  | 版本选择与更新检查 | `HomeService`、`ModUpdateService`、`CheckModUpdateTask` | Library + source provider + transfer workflow | 旧库的 `used_version`/available versions；合法 ZIP/PAK |
  | mod.io 来源 | `src/apis/modio`、`ModioSearchProvider`，固定 DRG game ID/URL | `mintcat.source.modio` | 契约 fixture；真实账号/受控 HTTP 服务尚缺 |
  | ModCat 来源 | `src/apis/modcat`、`ModcatSearchProvider`，DRG/RC game key 在前端选择 | `mintcat.source.modcat` | 契约 fixture；受控 HTTP 服务尚缺 |
  | Nexus 来源与授权 | `src/apis/nexusmods`、下载 WebView、FFI `nexus_webview` | `mintcat.source.nexusmods` + interaction broker | 契约 fixture；合法测试账号与人工授权环境尚缺 |
  | 本地来源 | `ModService.addModFromPath`、integrator 的 ZIP/PAK/目录分支 | `mintcat.source.local` | `fixtures/baseline/mod-inputs` 的合法小型 PAK、含 PAK ZIP、目录 |
  | 认证 | `AuthResolver` 从 `oauths.oauth` 读取明文 token | credential broker/keychain | 旧库仅含空 token，明确不含用户凭据；真实凭据迁移输入尚缺 |
  | 下载 | `DownloadApi` 的 Tauri downloadId + `ModUpdateService` 业务写库 | transfer/artifact runtime + Library gate | 小型 ZIP/PAK；并发交错输入由 R07/R15 增加 |
  | 安装 | `ModInstallTask` -> `IntegrateApi` -> `backend_invoke` -> integrator DLL | install workflow + game/engine workers + file transaction | 仅合成 Mod 输入；合法 DRG/RC 游戏 PAK/可恢复测试安装目录尚缺 |
  | 打包 | 当前安装器构建后直接部署，没有独立产品命令 | install + game adapter + engine，输出 artifact | 合成 Mod 输入；代表性真实游戏输入尚缺 |
  | 卸载 | `IntegrateApi.uninstall` -> integrator-core，按约定路径清理 | install + receipt/file transaction | 后续 R17 的临时 game layout；当前 fixture 不授权真实目录 |
  | Agent | `AgentService`/`AgentToolRegistry`/`ModAgentToolService` 和当前 CLI bridge | `mintcat.feature.agent`，只调 Application API | 现有 Agent/CLI 测试及 R01 wire fixture |
  | Proxy | `src/apis/proxyRuntime.ts` -> integrator DLL；源码实际在 `src-tauri/crates/mintcat-proxy` | `mintcat.feature.proxy` + broker | 源码可读；管理员权限、证书沙箱和授权系统环境尚缺 |
  | 备份/恢复 | `CloudBackupApi` 直接读取/替换 `mintcat.sqlite` | `mintcat.feature.backup` + owner snapshot/import | 无凭据旧库副本；受控备份服务与 WAL 一致性演练尚缺 |
  | frontend/runtime/资源更新 | `StartupUpdateCheckTask`、`frontend/update.rs`、`integrator/runtime.rs`、`hot_update.rs` | scoped update runtime + supervisor | 当前 manifest 形状；生产信任根、签名权限、隔离发布仓库尚缺 |
  | standard 发行 | `scripts/build-release.sh` + Tauri NSIS，Windows x86_64 | R27/R33 release tooling | 仅入口和配置；干净 Windows/签名环境尚缺 |
  | portable 发行 | `scripts/build-portable.mjs`，带 app/CLI/oodle/integrator | R33 同一 platform baseline | 仅入口和配置；干净 Windows 启动尚缺 |
  | fixed WebView2 发行 | `scripts/build-fixed-webview2.mjs` + `webview2.x64.json`；当前固定配置未列 CLI sidecar/integrator runtime | R33 同一 platform baseline | 仅入口和配置；CAB 工具、Windows 启动与产物内容验证尚缺 |

  外部位置与条件：

  - 更新分发当前指向杭州/新加坡 OSS `update.json`，整应用 updater 另指向 OSS/GitHub；MintCat API 为 `https://api.mintcat.work`。仓库没有发布控制服务源码，不能在本仓库假定服务端框架或生产发布权限。
  - Proxy 源码在 `src-tauri/crates/mintcat-proxy`；`scripts/package-proxy-runtime.mjs` 默认却解析根目录 `mintcat-proxy`，后续 R25/R33 必须统一，不能把找不到默认路径误报为源码缺失。
  - 仓库没有可合法提交的 DRG/RC 原始游戏 PAK、真实 Nexus/Mod.io/ModCat 测试凭据、生产 TUF/root key、Windows 签名/管理员测试环境。本卡只固定无凭据、无版权游戏数据的合成输入。

  实际产物：

  - `fixtures/baseline/manifest.json`
  - `fixtures/baseline/mod-inputs/baseline.pak`
  - `fixtures/baseline/mod-inputs/baseline.zip`
  - `fixtures/baseline/mod-inputs/directory-basic/pak/baseline.pak`
  - `fixtures/baseline/legacy/mintcat-v0.6.sqlite`
  - `src-tauri/crates/mintcat-integrator-core/tests/baseline_fixtures.rs`
  - `src-tauri/crates/mintcat-integrator-core/Cargo.toml`

  验证证据：

  - `cargo test --manifest-path src-tauri/Cargo.toml -p mintcat-integrator-core --test baseline_fixtures` -> 2 tests passed；实际由当前 `repak` reader 读取 PAK/ZIP 内 PAK，由当前目录 validator 接受目录，并以只读 SQLite 连接核对 integrity、两游戏、Profile 顺序/启停/版本以及无凭据约束。
  - 未运行真实游戏安装、系统证书/Proxy、生产热更新或三种 Windows 发行产物；以上外部条件明确保留给对应卡片，不记为通过。

  限制/阻塞：R00 无代码阻塞；真实游戏/授权/签名/目标平台输入按上列条件阻塞其后续端到端验收。

- [x] **R01 — 建立唯一契约事实源与生成链**
  - 前置：R00。
  - 目标：`packages/contracts`、Rust/TS SDK 生成入口；消费现有 `mintcat-agent-protocol` 与 integrator-api 的有效语义。
  - 动作：定义 CommandSpec、JSON-RPC envelope、AppError、Job/Action、ModRef、Profile revision/incarnation、ArtifactRef、ModuleManifest、ScopeReleaseSet/EnvironmentLock 和 scoped provider binding。
  - 动作：加入 `cache-only/prepare-and-apply`、fencing、contentRevision、gameId/installationId、包/契约/schema 三种版本；JSON Schema 生成 Rust DTO、TS 类型与客户端和 CLI/Agent 参数元信息。
  - 验收：真实编码/解码边界覆盖合法输入、未知方法、错误字段类型、版本不兼容；生成后 Rust/TS 对同一消费者 fixture 语义一致。
  - 删除条件：每个迁移完成的 wire DTO 不再手工维护第二份 TS/Rust 类型；OpenAI tool schema 只是生成目标，不是业务事实源。

  执行状态：已完成

  负责人：omp coding assistant

  契约与生成边界：

  - `packages/contracts/schema/core.schema.json` 是 wire DTO 的唯一事实源；覆盖 JSON-RPC request/response、`AppError`、Job/Action、显式来源 `ModRef`、Profile revision/contentRevision/incarnation、artifact digest、operation fence、模块 manifest、scope release/environment lock 及 scoped provider binding。
  - `packages/contracts/commands.json` 是 `CommandSpec` 事实源；当前 15 个 Application API 命令明确 contractVersion、执行模式、权限、幂等、确认策略、错误码、CLI 路径和 Agent 暴露策略。`command-catalog.schema.json` 固定目录结构，生成器额外拒绝重复 method/CLI path、开放参数对象和无幂等声明的 mutation。
  - 版本维度显式分为 package version、package schema version、contract version 与 owner data schema version；下载意图分为 `cache-only`/`prepare-and-apply`，安装命令同时携带明确 `gameId`/`installationId`。
  - `packages/contracts/scripts/generate.mjs` 只从上述 JSON 源生成 Rust DTO/descriptor、TS DTO/typed client，以及内联参数 schema 的 CLI/Agent metadata；OpenAI tool schema 位于生成产物，不作为输入。`--check` 会拒绝陈旧产物，Rust 产物经同一生成入口调用 `rustfmt`。
  - `packages/sdk-rust` 与 `packages/sdk-ts` 在反序列化前执行同一 schema 边界，区分 malformed request、unknown method、incompatible contract、invalid argument/result，并在 mutation 入口检查幂等键。当前旧 Agent/integrator wire 类型尚未宣称迁移；其消费者 cutover 分属后续卡片，避免在 R01 同时维护第二份 vNext DTO。

  实际产物：

  - `packages/contracts/schema/core.schema.json`
  - `packages/contracts/schema/command-catalog.schema.json`
  - `packages/contracts/commands.json`
  - `packages/contracts/generated/command-metadata.json`
  - `packages/contracts/scripts/generate.mjs`
  - `packages/sdk-rust/src/generated.rs`、`packages/sdk-rust/src/lib.rs`
  - `packages/sdk-ts/src/generated.ts`、`packages/sdk-ts/src/index.ts`
  - `fixtures/contracts/rpc-boundaries.json`

  验证证据：

  - `pnpm contracts:check` -> 生成产物与事实源一致。
  - `pnpm exec tsc -p packages/sdk-ts/tsconfig.json` -> TS SDK 严格类型检查通过。
  - `pnpm exec vitest run packages/sdk-ts/src/contracts.test.ts` -> 7 tests passed；覆盖合法请求、未知方法、错误字段类型、未知参数、版本不兼容和结果边界。
  - `cargo test --manifest-path packages/sdk-rust/Cargo.toml` -> 2 tests passed；Rust 使用与 TS 相同的 `rpc-boundaries.json` 并得到相同错误码语义。
  - `cargo clippy --manifest-path packages/sdk-rust/Cargo.toml --all-targets -- -D warnings`、`cargo fmt --manifest-path packages/sdk-rust/Cargo.toml -- --check` -> lint 与格式门禁通过。
  - 实际 smoke 分别解码 `mods.setVersion`：TS 输出 `{\"method\":\"mods.setVersion\",\"profileId\":\"profile-1\",\"revision\":7}`，Rust 输出 `mods.setVersion:profile-1:7`；临时 smoke 文件已删除。

  限制/阻塞：R01 无代码阻塞；工作区接入与旧消费者切换分别由 R02 及对应业务 cutover 卡片完成。

- [x] **R02 — 建立可运行的目标工作区与隔离开发入口**
  - 前置：R01。
  - 目标：Cargo/pnpm workspace、`apps/desktop`、`apps/cli`、`runtime/daemon` 和共享 SDK 的真实构建入口。
  - 动作：保留现有应用可用路径；给 vNext 使用独立 data-dir 和明确界面标识；统一目标 triple、构建输出和命令约定。
  - 验收：实际启动 CLI/daemon 基础程序与桌面开发入口；新实例不打开真实旧数据库，不修改游戏目录。
  - 限制：本卡只建立执行环境，不能据此宣称插件架构已完成；不得提交空的业务 handler 假装下一卡已实现。

  执行状态：已完成

  负责人：omp coding assistant

  实现：

  - 根 `Cargo.toml` 建立独立 vNext workspace，纳入 `apps/cli`、`runtime/daemon`、`runtime/kernel` 与 Rust SDK；旧 `src-tauri` workspace 和现有应用入口保持不变。
  - `pnpm-workspace.yaml` 纳入 `apps/*`、`packages/*`、`modules/*`、`tools/*`；新增 `vnext:target/check/cli/daemon/desktop` 统一命令。`scripts/vnext-target.mjs` 在当前主机解析精确 target triple 和共同输出位置，darwin arm64 实测为 `aarch64-apple-darwin`。
  - `runtime/kernel` 要求显式 `--data-dir`/`MINTCAT_VNEXT_DATA_DIR`，以 `.mintcat-vnext.json` 标识 layout；未标记且含 `mintcat.sqlite` 的目录会被拒绝。初始化只创建 vNext runtime/artifact 目录，不推断旧 `com.mint.cat` 路径。
  - `mintcatd init/paths` 与 `mintcat environment` 是可运行的基础入口；只提供本卡真实具备的布局初始化/检查能力，没有伪造 daemon 业务 handler。
  - `apps/desktop` 提供端口 1430 的独立开发界面，明确显示 vNext、隔离 data-dir、daemon 离线状态和旧应用端口 1420；旧 Tauri 开发入口未替换。

  验证证据：

  - `cargo test --workspace` -> 5 tests passed；包括显式目录初始化、旧库拒绝、错误 product marker 拒绝和 R01 契约边界。
  - `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` -> 通过。
  - `pnpm --filter @mintcat/desktop-vnext check` 与 `build` -> 通过，Vite 实际生成 `apps/desktop/dist`。
  - CLI/daemon smoke 在 `target/r02-smoke` 初始化 vNext，二者输出同一 layout；相邻 legacy sentinel 校验输出 `legacy-untouched:true`。
  - Chromium 实际打开 `http://127.0.0.1:1430`；界面显示 `/tmp/mintcat-vnext-r02`、`Offline`、`Preserved on port 1420`，console errors 为空。

  限制/阻塞：R02 无代码阻塞；IPC、真实 status/doctor 和按需启动由 R03 实现，本卡未冒充完成。

- [x] **R03 — 实现独立 daemon、本地 IPC 和生命周期**
  - 前置：R02。
  - 目标：`runtime/daemon`、本地传输、CLI/桌面连接器；取代桌面内 loopback bridge 的运行职责。
  - 动作：Unix socket/Windows pipe、peer/ACL 验证、data-dir 单实例锁、握手、请求关联、大小/deadline 边界、断连处理与按需启动。
  - 动作：实现真实 `status/doctor`；status 不偷偷启动，业务连接可启动 daemon；无活跃工作时才能退出，关闭 GUI 不终止 daemon 工作。
  - 验收：无 GUI 启动/连接、两客户端共用实例、过期端点、伪造身份、不同 data-dir 隔离、断连重连；检查 stdout/stderr 分离。
  - 禁止：默认开启远程 TCP、把 token 放进命令行、把 PID 存在视为认证。

  执行状态：已完成

  负责人：omp coding assistant

  实现：

  - `runtime/ipc` 提供 4-byte 长度前缀的 JSON 帧，限制 1 MiB，具有握手、协议版本、generation、请求 id 关联、请求 deadline、握手/空闲 deadline 和断连处理；daemon 收到关闭信号后先等活动请求归零。
  - Unix 传输使用 data-dir 内 mode `0600` socket 和 mode `0700` runtime 目录，通过内核 peer credentials 校验有效 UID；Windows 使用拒绝远程客户端的 owner named pipe，并用 `GetNamedPipeClientProcessId` 校验声明 PID。PID 文件只作锁诊断，认证不依赖 PID 存在。
  - 每个 data-dir 使用独占文件锁和独立 endpoint/generation；取得锁后才清理过期端点。共享 `connect_or_start` 只为业务连接初始化/拉起 daemon，日志写入 `runtime/daemon.log`；`status` 只连接，缺席时既不创建目录也不启动进程。
  - `runtime/daemon` 实现真实 `system.status`/`system.doctor`，doctor 检查 marker、目录可写性、endpoint 权限和 peer authentication；未迁移命令明确返回 `MODULE_UNAVAILABLE`。CLI stdout 只输出响应 JSON，诊断和 daemon 日志走 stderr/日志文件。
  - `apps/desktop/src-tauri` 是实际 Tauri host，复用同一 Rust IPC 连接器；网页端通过生成 TS SDK 校验 status/doctor 结果。浏览器预览不会伪装 native IPC，原生界面提供“不启动的 status”和“按需启动并诊断”两条明确动作。

  验证证据：

  - `cargo test --workspace` -> 14 tests passed；覆盖帧上限、伪造 peer 身份、请求 deadline、同目录第二实例、过期端点、两客户端同 generation/请求关联、过期 session、断连重连、doctor、两 data-dir 隔离以及原生桌面连接器。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` -> 通过；`cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings` -> Windows named-pipe 路径通过交叉编译门禁。
  - 实际无 GUI 进程 smoke：缺席 `status` 非零退出且未创建 data-dir；`doctor` 启动 daemon 并返回 healthy；两个独立 `status` 进程得到 generation `d996bbde-8f9b-4c3d-9f23-a73f8001d9e1`；SIGTERM 后 endpoint 清理且后续 status 不重启；stdout 均为单行 JSON，legacy sentinel 未改变。
  - `pnpm contracts:check`、TS SDK 7 tests、desktop TypeScript check/build -> 通过；Tauri Rust/JS 依赖对齐至 2.9.1，`pnpm vnext:desktop:native` 实际编译并启动 `target/debug/mintcat-desktop-vnext`，无版本不匹配告警。

  限制/阻塞：R03 无代码阻塞；当前 daemon 只承载 system 状态/诊断，worker 工件、业务 jobs 和其他模块分别从 R04/R05 开始接入，未迁移调用不会回退到 legacy 实现。

- [x] **R04 — 实现模块 supervisor、worker SDK 和内容工件基础**
  - 前置：R01、R03。
  - 目标：`runtime/supervisor`、`runtime/artifacts`、`packages/sdk-rust`。
  - 动作：按 digest 的不可变工件、manifest/文件清单/target 校验、真实密码学信任校验、继承 pipe 启动、握手/health、scoped contract registry、实例生命周期和 draining。
  - 动作：大文件用 grant/artifact handle，日志与协议分流；无状态 worker 可多版本实例；实现工件租约和 GC 的基础引用，不原地覆盖运行中包。
  - 验收：真实 worker 执行文件/计算工作；崩溃不带走 daemon；错 target/契约/签名拒绝执行；运行任务引用的工件不能清理。
  - 信任：开发签名链只在隔离 dev mode 生效；production 不通过跳过验签解决配置缺失。

  执行状态：已完成

  负责人：omp coding assistant

  实现：

  - `runtime/artifacts` 将模块发布到 `artifacts/modules/sha256/<digest>`：发布前逐项拒绝绝对/父目录/symlink/额外文件，流式核对 size/SHA-256，校验 manifest schema、精确 target、host API、worker entry 与 contract 唯一性；临时目录同步后原子 rename，已发布树只读且同 digest 不原地覆盖。
  - 包签名使用 Ed25519 对 domain-separated canonical package digest 做真实验签。production/test key 与 development key 分表；development key 只在显式 `RuntimeEnvironment::Development` 生效，production 缺失 key 时 fail closed，不存在 debug 跳过验签。
  - blob 工件同样按 SHA-256 不可变发布；`ArtifactGrantDescriptor` 只传受租约保护的只读 handle 元数据，worker 流式读文件而不把大内容塞进协议。模块实例与 grant 都持有 RAII lease，GC 在同一 lease registry 下跳过运行中引用。
  - `packages/sdk-rust` 实现 1 MiB 上限的 inherited stdin/stdout framed worker protocol、manifest-bound hello/host hello、health/invoke/shutdown 与结构化错误；stdout 只承载协议，stderr 单独写入 `runtime/module-logs/<instance>.stderr.log`。
  - `runtime/supervisor` 按 `(scopeId, contractId, contractVersion)` 注册已验签 provider，启动前检查 required/provided contracts；真实子进程 health 成功后才 active。实例具有 active/draining/stopped/crashed 生命周期，drain 阻止新请求并等待活动请求；stateless 模块可同时运行不同 digest/version，子进程崩溃只标记该实例。
  - daemon 初始化时持有 ArtifactStore/ModuleSupervisor，并在 IPC 停止后 drain 所有 worker。契约事实源新增 signature、artifact grant 和 worker wire DTO，contract set 提升至 1.2.0。

  验证证据：

  - `cargo test --workspace` -> 23 tests passed；artifact tests 覆盖错误 target、篡改签名、未列/变更文件、dev key 的 production 拒绝、immutable publish、lease/GC；supervisor 使用真实 `mintcat-worker-example` 覆盖两版本并存、计算、文件 grant、contract 拒绝、进程 crash 隔离、drain 和回收。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings` -> native/Windows 门禁通过。
  - 直接进程 smoke 与真实 worker 完成 protocol 1 握手、health、`math.sum=9.5`、20-byte grant 的 SHA-256 `ea1f4c49595502b3f2bab57215bee97faba19f3ad9e29372f5cbe0d7f93ed964` 和 shutdown；确认 stdout 无额外字节、stderr 日志分流、exit 0。
  - daemon-owned supervisor smoke：`doctor` healthy，runtime 创建独立 module log 目录，`status` generation `5ef67bff-8e86-4eeb-b298-7d1427d0e0f6`，SIGTERM 后 supervisor/IPC 清洁退出。
  - `pnpm contracts:check`、TS SDK 7 tests、desktop check/build -> 通过；Rust/TS worker DTO 与生成产物一致。

  限制/阻塞：R04 无代码阻塞；本卡提供可信执行/工件底座，不冒充 R05 的持久 job/checkpoint，也不提前实现 R27 的发布打包和 release-set 激活。

- [x] **R05 — 实现持久 jobs、action、事件和操作关联**
  - 前置：R01、R03。
  - 目标：`runtime/jobs`、kernel 数据库和客户端 job API。
  - 动作：持久登记后返回 jobId；按 job 的 seq、snapshot/afterSeq、有限事件保留、幂等 operation 映射、取消请求/安全点、worker checkpoint 和恢复状态。
  - 动作：持久 ActionRequired/批准策略/消费结果；确认绑定目标、revision、planHash、module lock；区分客户端超时、取消请求和真实终态。
  - 验收：响应丢失后找回 job、重连事件去重、GUI 关闭继续运行、进程重启恢复、不把未知提交状态重新从头执行。
  - 禁止：永久依赖 JS Map，或以 kill worker 代替业务取消/回滚。

  执行状态：已完成

  负责人：omp coding assistant

  实现：

  - `runtime/kernel` 增加隔离 `kernel.sqlite` 的单连接协调器、WAL/FULL 同步、外键、busy timeout 与按 owner/version 的事务迁移；`runtime/jobs` 以同库事务持久化 jobs、单调 per-job seq events、operations 和 actions。job 与 idempotency 映射提交成功后才返回 `jobId`，同 key/同输入重放原 job，同 key/不同输入返回冲突。
  - `JobStore` 提供 snapshot、`afterSeq` 分页和有限保留；游标早于保留窗口时同时返回权威 snapshot、`retentionStartSeq` 与 `resetRequired`。取消只持久化 request 并进入 `cancelling`，worker 仅在无进行中/未知 commit 的安全点写入终态；客户端 deadline 不修改 job。
  - worker checkpoint 持久化 step/payload/resume policy。daemon 启动扫描未完成 job：有显式 resumable checkpoint 的 job 经过 `recovery-required` 后重新排队；无安全 checkpoint 的 job 保持 `recovery-required`；commit in-progress/unknown 固定为待检查，绝不从头执行。
  - ActionRequired/approval/response/consume 全部落库；approval 具 revision 与幂等映射，消费前逐项核对 target、显式 target revision、plan digest、module-lock digest、有效期和 response schema。契约事实源新增 `jobs.watch`、`actions.get`、`JobEventPage` 和完整 action binding，contract set 提升至 1.3.0。
  - daemon 在绑定 IPC 前打开 job store 并执行恢复；`jobs.get`、`jobs.watch`、`jobs.cancel`、`actions.get`、`actions.approve` 走同一生成契约和 OS-authenticated local owner scope。`BoundDaemon::job_store` 为后续业务 module 登记/驱动 job 提供单一 durable owner；没有 JS Map 或 worker-kill 取消旁路。

  验证证据：

  - `cargo test --workspace` -> 30 tests passed；覆盖响应丢失后的 registration replay、operation 冲突、事件重连/截断 reset、取消请求与安全点、commit authority、action revision/target revision/plan/module lock、approval replay、checkpoint 重启恢复和 unknown commit 隔离。
  - daemon Unix IPC 集成场景通过真实 socket 执行 get/watch/cancel/action get/approve；断开客户端后 job 仍为 `cancelling`，重连可读，daemon 重启后无 checkpoint job 为 `recovery-required`，已批准 action 仍为 `approved`。
  - 独立可执行 smoke（临时 harness 已删除）观察到 `initialState=running`、`retainedEvents=2`、cancel response/reconnect 均为 `cancelling`、daemon restart 为 `recovery-required`；证明客户端关闭没有终止 daemon job，未知恢复未伪装成成功或重新开始。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 通过；Windows target clippy 使用 MinGW C compiler 与 LLVM MSVC archiver 通过，cross C helper 报告 GNU/MSVC compatibility warning，实际 Windows 运行留给 R34 平台矩阵。
  - `pnpm contracts:check`、TS SDK 7 tests/check、vNext desktop check/build 通过；生成的 Rust/TS job/action DTO 与命令 metadata 无漂移。

  限制/阻塞：R05 无代码阻塞；本卡提供 durable orchestration 与客户端查询/批准入口，具体 Library transaction/fence、transfer 和 file journal 分别由 R07、R14、R17 接入，未把其提交语义假装为已实现。

- [x] **R06 — 建立 Library 单写者与旧数据导入能力**
  - 前置：R01、R03。
  - 目标：`modules/library/service`；迁移 `src/storage/db`、DAO 中的业务持久模型。
  - 动作：定义游戏定义/安装实例、Profile/分组/Mod、显式来源身份、多版本内容引用、game-scoped settings、contentRevision/incarnation；令牌改存 keychain 引用。
  - 动作：实现一致 backup/export/import 与 legacy ID 映射；保留顺序、启用、usedVersion 和旧库，不把旧 installedHash 冒充 receipt。
  - 验收：在旧库副本上导入，逐项核对引用、顺序、版本和身份；两游戏设置相互独立；客户端无直接 SQL 写权限。
  - 禁止：复制忽略 WAL 的 SQLite 文件当一致备份；开放新旧库双写。

  执行状态：已完成

  负责人：omp coding assistant

  实现：

  - 新增 `modules/library/service`，由 daemon 独占 `state/mintcat.library/library.sqlite` 的连接和迁移。Library 与 kernel jobs 分库；公开 API 只提供领域 snapshot、game/global setting、legacy import、export/restore，不向 IPC 客户端或其他模块暴露 SQL connection。
  - schema 分离 game definition/installation、Profile/group/profile_mod、Mod source identity/version content reference、preference、credential reference、legacy mapping/import run。Profile 具有 `revision`/`contentRevision`/incarnation，membership 具有独立 incarnation；来源以字符串 `sourceId/remoteGameKey/remoteModId` 标识，版本可以并存。
  - legacy importer 对显式路径以 read-only SQLite transaction 读取，先做表与 integrity 校验；canonical rows 的 SHA-256 作为幂等 source identity。它保留 folder parent、sort order、enabled、`usedVersion`、状态、下载元数据和 legacy ID 映射；旧 cache path 只标 `legacy-unverified`，旧 `installedHash` 只进入 `unverified` hint，均不冒充 artifact/receipt。
  - 全局与按 game ID 的 setting 具有独立复合键。OAuth 明文永不写 Library；非空 token 必须经 `CredentialVault` 得到 opaque keychain reference，事务失败会回收已暂存 reference。检查入 fixture 无真实凭据，R09 broker 提供生产 OS keychain 实现。
  - export 在一个读事务内构建确定性逻辑 snapshot，写入同目录临时文件并 sync，再用 no-clobber hard link 原子发布；restore 只接受版本化格式和空 Library。没有复制可能遗漏 WAL 的 `.sqlite`，也没有对 legacy 数据库双写。
  - `RuntimeLayout` 增加隔离 `state/`；Unix Library state/database 权限分别为 `0700`/`0600`。daemon 在服务 IPC 前创建 Library owner，重启复用同一 module-private DB。

  验证证据：

  - `cargo test --workspace` -> 32 tests passed。Library 测试复制并导入 `fixtures/baseline/legacy/mintcat-v0.6.sqlite`，在未 checkpoint 的 WAL 中追加 setting 后仍可见；核对 2 games/2 installations/2 profiles/3 groups/3 mods/4 versions/3 memberships、parent/order/enable/usedVersion/source identity、全部 legacy mapping 和 2 个 unverified install hint。
  - 同一测试证明 DRG/RC 的同名 setting 分别保持 `drg-value`/`rc-value`，重复导入按 source digest 返回原结果，源数据库主文件 digest 不变；不存在旧库写回或 active-game 推断。
  - 合成非空 OAuth `super-secret-token` 只到 vault 测试边界，Library DB/WAL 与导出的 backup 均不含该字节，只含 `keychain://test/modio/1`；export -> 新 data dir restore 后 snapshot 逐项相等，现有 backup 和非空 Library 均拒绝覆盖。
  - 独立 daemon smoke（临时 harness 已删除）导入真实旧库后经 IPC `system.status` 保持服务，重启读取 `restoredProfiles=2`；观察路径为 `state/mintcat.library/library.sqlite`，输出 `games=2, profiles=2, groups=3, mods=3, profileMods=3, legacyHintsAreReceipts=false`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、vNext check、TS SDK 7 tests、desktop build 通过；Windows target clippy 同 R05 的 C cross 配置通过，仅保留 GNU/MSVC helper warning。

  限制/阻塞：R06 无代码阻塞；真实 credential 输入按 R00 明确缺失，生产 OS keychain adapter 属 R09。R07 才加入同库 gate/fence/outbox，R08 才迁移 GUI/CLI/Agent Library CRUD 消费者；本卡没有让未迁消费者回退写 vNext Library。

- [x] **R07 — 实现原子能力门禁、fencing 和同库 outbox**
  - 前置：R05、R06。
  - 目标：Library transaction/OperationGate；参考下载专项第 5–9 节。
  - 动作：同一短事务完成权限/目标/revision 检查、Profile 内容 lease 获取、fence 推进、operation/idempotency/outbox 登记；普通编辑也在同库检查门禁再写。
  - 动作：取消先撤销提交权再释放占用；旧 fence/旧 incarnation 拒绝；outbox 重放关联同一个 kernel job；心跳过期不能直接删锁。
  - 验收：用确定性 barrier 交错下载-start 与编辑/删除、两个客户端争用、取消与完成、owner 重启和迟到回调，检查最终领域数据而不是锁布尔值。
  - 强制：门禁是后端权威；任何 GUI/CLI/Agent 写入口不能绕过。

  执行记录（2026-09-30）：

  - Library schema v2 在 owner 数据库内加入 download operation、Profile-content lease、单调 `lease_epoch`、固定 item 输入、verified content ref/binding、普通 mutation idempotency 与 outbox。`start_download_operation` 在同一 `BEGIN IMMEDIATE` 事务内核对后端权限/owner、expectedContentRevision、Profile/membership incarnation、source/version identity 和资源占用，再推进 fence 并登记 operation/items/outbox；启停与删除也在其写事务内检查相同 gate。
  - apply 只接受 `ArtifactStore::grant_blob` 产生且仍持有 lease 的 `ArtifactGrant`。整批重新核对 operation fence/generation、Profile revision/incarnation 和每个 membership/source/version pin 后，才原子写 verified content ref、binding、usedVersion、content revision、terminal result 与应用事件。取消事务先推进 Profile lease epoch，再把 lease 标为 revoked 和 operation 标为 cancelled；旧 fence、旧 owner generation 与迟到回调均不能提交。
  - heartbeat 超时和 daemon generation 变化只把 active lease 标为 `suspect`、operation 标为 `recovery-required`，不会删除或自动释放；显式 resume 核对当前 revision 后产生新 fence。capability 查询区分 busy 与 recovery-required。
  - daemon 启动恢复 Library operation，并按 `event_seq` 重放 `kernel-job`/`kernel-job-control` outbox。下载以 Library operation ID 作为 kernel job idempotency key、以冻结 inputs hash 作为 request hash；只有 kernel job/取消请求持久化后才标记 delivered，重复重放返回同一 job。逻辑 backup/restore 已覆盖 operation、lease、items、content refs/bindings、mutation results 和 outbox，不丢提交权状态。

  验证证据：

  - `cargo test --workspace` -> 36 tests passed。确定性 `Barrier` 测试交错 start/edit、start/delete、两个客户端争用、cancel/apply；逐项断言最终 content revision、membership、binding 与 terminal operation，而不是只检查锁标志。同组还覆盖相同/冲突 idempotency key、无权限写、取消后的迟到 callback、owner restart -> suspect -> explicit resume、新旧 fence 和完整 gate snapshot export/restore。
  - daemon outbox 测试证明首次 replay 只注册一个 `library.download.apply` job，第二次 replay 为 0；取消后 control outbox 把同一 job 推进 `cancelling`，再次 replay 仍为 0。
  - 独立 Library smoke（临时 example 已删除）真实导入 baseline、发布并 grant artifact、start、幂等 replay、apply 和读取 binding，输出 `operationCreated=true, replayCreated=false, sameOperation=true, applied=true, contentRevision=2, pendingKernelEvents=1`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R07 无代码阻塞。当前权威 gate/outbox API 只在 backend crate 与 daemon composition 中；R08 才把完整 Library IPC/GUI/CLI/Agent 查询和编辑入口切到该 owner，R15 才接 transfer/source provider 的真实下载执行，未迁入口不得直接取得 Library connection。

- [x] **R08 — 完整迁移 Library 查询和编辑用例**
  - 前置：R06、R07。
  - 目标：`HomeService`、`ProfileService`、`ModService`、`ProfileTreeService`、配置导入及其 GUI/CLI/Agent 消费者。
  - 动作：Profile/分组/列表 CRUD、分页、排序、移动、批量启停、版本意图、rename/remove 语义；用明确 ID 和 expectedContentRevision，不重新读取 active Profile 决定写入目标。
  - 动作：查询不隐式创建/激活 Profile；保留共享名称与仅移除 Profile 关联的既有语义；缺失本地 Mod 清理按明确游戏/Profile 范围执行。
  - 验收：无 GUI 完成查询与编辑，GUI 后连接显示一致；冲突不丢写；排序保持 first-wins；Profile 切换不改变已发请求目标。
  - cutover：逐用例同时迁移所有消费者并删除旧业务实现；未迁能力不得回退写真实旧数据库。

  执行记录（2026-04-12）：

  - contracts v1.4 从同一 `CommandSpec` 生成 Rust/TypeScript DTO、CLI 路径和 Agent 暴露元数据：`games.list`，Profile/分组 CRUD，分页 Mod list/get，共享实体 rename，单个/批量启停，first-wins move，仅移除 Profile 关联，明确 game/Profile 范围的 missing-local cleanup，以及独立于生效 binding 的版本意图。
  - `LibraryStore` schema v3 加入 Profile tombstone、Mod entity revision/incarnation 和 membership version selection。所有 query 都要求 owner 与明确 ID，使用与 query/snapshot 绑定的不透明 cursor，且不隐式创建或激活 Profile；所有 mutation 在 `BEGIN IMMEDIATE` 内执行权限、expected revision、相同 Profile-content lease gate、幂等记录、变更和 application outbox。
  - Profile clone 固定复制来源 revision 下的分组、顺序、启停和版本选择；active Profile 删除不会自动选择替代项。共享 rename 修改 Mod 实体，remove/cleanup 只删除目标 Profile 关联并保留共享 metadata/content。版本意图只更新 requested selection，再通过 Library outbox 注册持久 `mods.setVersion` job，R15 接管实际下载与 apply。
  - daemon 为每条命令提供 typed IPC handler。原生 desktop bridge 可调用任意 catalog command，Library 面板从明确 Profile 读取分页列表并以捕获的 Profile ID/content revision 发出启停；Profile 切换不会改写已发请求。vNext CLI `call` 和 Agent catalog 使用相同生成契约，不取得 SQL connection；R23/R24/R32 分别扩充完整产品 UI、Agent 会话 adapter 和人类友好领域 CLI。

  验证证据：

  - `cargo test --workspace` -> 40 tests passed。Library 行为测试覆盖 side-effect-free cursor 分页、stale cursor、显式 Profile、clone/order、共享 rename、批量启停、move、版本意图、关联 remove、按 game/Profile 清理 missing-local、无隐式激活，以及每个 content command 的 backend gate；daemon IPC 测试覆盖相同查询/编辑和版本 job。
  - 实际 vNext CLI 启动 sibling daemon 后调用 `games.list`，得到有效 JSON-RPC `{"items":[]}`；无 GUI 路径不依赖 renderer。desktop native connector 测试命中同一 authenticated daemon；Vite/Tauri API 表面实际加载 game/Profile/两条 Mod，并执行 SkyUI toggle，界面从 `revision 7` 更新为 `Saved revision 8`。
  - TypeScript contract 测试 9 项通过并断言全部 Library commands 由 Agent catalog 暴露，missing-local cleanup 缺 game scope 会在边界拒绝；desktop production build 和 `tsc --noEmit` 通过；`pnpm contracts:check` 确认生成物无漂移。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R08 无代码阻塞。`mods.setVersion` 目前只持久化意图/job，真实 provider 下载与原子 apply 属 R10–R15；完整 workbench、Agent 会话/审批 adapter 和产品领域 CLI 分别属于 R23、R24、R32。旧 Tauri 应用继续隔离保留，未回退写其数据库。

### 第二组：来源、下载与列表一致性

- [x] **R09 — 实现网络、凭据和受控交互 broker**
  - 前置：R03、R04。
  - 目标：迁移 `services/network` 和 OS credential/webview 原语；`runtime/broker`。
  - 动作：统一代理路由、凭据注入、来源域名范围、redirect 校验、脱敏和取消；文件选择/授权返回 grant，不把任意路径当权限。
  - 动作：受控浏览器/文件交互转换为 ActionRequired，headless 下可以报告并继续流程，不偷偷要求后台 renderer 存在。
  - 验收：受控本地服务或授权测试账号验证凭据不跨域转发、重定向规则、代理选择、交互中断/恢复与结构化错误。
  - 禁止：每个 provider 自写一套 token/代理逻辑，或提供任意 shell 的提权接口。

  执行记录（2026-04-12）：

  - 新增 `runtime/broker`。`NetworkBroker` 对每个请求强制精确 HTTP(S) origin scope、手动 redirect 校验、超时/取消、响应大小上限和 direct/forward fallback；forward proxy 不接收 provider credential。请求方不能直接提交 authorization/cookie/API-key header，credential 只能由 opaque `CredentialRef` 经 declared origin scope 注入。
  - `OsCredentialStore` 使用平台 keyring，测试与受控环境使用同一 `CredentialStore` contract；secret 无 `Debug` 明文并在 drop 时清零。网络错误只保留 request ID、无 query 的 endpoint、类别/status，不记录 credential、query token 或 response cookie。
  - 浏览授权和文件选择均创建已有 kernel 中的 durable `ActionRequired`。`InteractionHost` 逐次验证 authorization navigation/callback origin；选择文件只生成持久、限时、一次性 grant，consumer 只能让 broker 打开文件，不能把任意 path 当 capability。action/grant 跨 broker restart 后仍可恢复，headless 不依赖 renderer。
  - legacy `services/network` 不再作为 vNext provider 的回退入口；R10–R13 source module 只能接收 broker API。受控交互没有 shell/elevation primitive。

  验证证据：

  - `cargo test --workspace` -> 46 tests passed，其中 broker 6 项在真实 loopback TCP 服务上证明 credential 在跨 origin redirect 时被剥离、越界 redirect 拒绝且诊断脱敏、direct 失败按策略走 forward proxy、in-flight cancellation；持久 action 测试证明错误扩展名/恶意 callback/越界导航拒绝，以及 restart 后 one-time file/callback grant 消费。
  - 独立 broker smoke（临时 example 已删除）真实向 loopback HTTP 服务注入 keychain-contract bearer credential，输出 `status=200, route=direct, credentialInjected=true, body=ok`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R09 无代码阻塞。R00 没有授权测试账号，因此按验收允许项使用受控本地服务而未触碰真实 credential；R10–R12 分别负责把 mod.io/ModCat/Nexus provider 与 Nexus host webview 接到本 broker，R25 才管理 proxy 长期 service lease。

- [x] **R10 — 迁移 mod.io 来源模块**
  - 前置：R09。
  - 目标：`src/apis/modio`、对应 search provider -> `modules/source-modio`。
  - 动作：实现搜索分页、详情/版本/依赖、解析具体 remoteFile、下载描述、审核/标签映射；剥离 Profile/界面状态。
  - 验收：同一明确 game binding 的搜索/版本/下载定位和错误行为；过期 token、不可用 Mod、指定旧版本不被解析成 latest。
  - cutover：搜索、添加、更新检查、CLI/Agent 使用同一 provider；删除静态 provider 分支。

  执行记录（2026-04-12）：

  - 新增 `modules/source/api` 的共享 `SourceProvider`/registry，以及无 Profile/UI 状态的 `modules/source-modio`。全部入口要求明确 `SourceGameBinding {gameId, remoteGameKey, providerGameId}`；URL parser 同时核对 mod.io path 中的 game key。
  - provider 通过 R09 `NetworkBroker` 完成 bearer 注入、origin/redirect/proxy/cancellation policy；实现 live cursor 搜索、query/sort 映射、detail、全部 files/version、dependencies、review/maturity/tag/stat 映射和受限 download descriptor。分页 cursor 绑定来源、游戏、query、page size 与排序。
  - 长期身份为 `ModRef`，稳定版本为 `file:<remoteFileId>`。download resolution 必须携带 versionId 或 remoteFileId，精确选择旧 file；legacy display version 只有唯一匹配时可用，歧义会拒绝，绝不回退 latest。短期 `binary_url` 只存在 `DownloadDescriptor`，并且必须属于配置的 CDN origin。
  - vNext search/add/update/CLI/Agent 后续 consumer 只能经共享 registry 取得同一 provider，不存在第二套静态分支；隔离的旧 Tauri 源文件按 R00 保留到 R34，不被 vNext 调用。

  验证证据：

  - `cargo test --workspace` -> 50 tests passed。mod.io 4 项 loopback 契约测试覆盖明确 DRG game binding、分页/sort/cursor mismatch、详情/标签/审核映射、版本/依赖、精确旧 file、越界 CDN、pending Mod、过期 bearer 401 和跨 game link 拒绝。
  - 独立 provider smoke（临时 example 已删除）经真实 `NetworkBroker` 查询 loopback mod.io fixture，输出 `requestBoundToGame=true, remoteModId=42, name=\"Smoke Mod\", total=1`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R10 无代码阻塞。R00 没有 mod.io 授权测试账号，按既定输入使用受控 HTTP 契约服务；R14/R15 消费 download descriptor，R23/R24/R32 分别提供最终 UI/Agent/领域 CLI 表面。

- [x] **R11 — 迁移 ModCat 来源模块**
  - 前置：R09。
  - 目标：`src/apis/modcat`、对应 search provider -> `modules/source-modcat`。
  - 动作：将 ModId/FilesId/VersionNumber 等来源语义封装在 provider，统一为 ModRef/remoteFile；游戏映射来自 game descriptor。
  - 验收：DRG/RC 不混 game-key，同一 Mod 多个文件版本得到不同正确文件身份；网络/授权错误不假成功。
  - cutover：删除前端版本切换组件内拼接 ModCat 下载地址的逻辑。

  执行记录（2026-04-12）：

  - 新增 `modules/source-modcat`，实现共享 `SourceProvider`。search cursor 绑定明确 `SourceGameBinding`，POST body 始终携带 descriptor 给出的 ModCat `GameId`；detail 同时核对返回 `GameId` 和请求 `ModId`，DRG `drg` 与 Rogue Core `drgrc` 不共享静态选择状态。
  - `ModId`、`VersionId`、`FilesId` 分别映射到 `remoteModId`、`versionId`、`remoteFileId`。版本列表保留同一 Mod 的每个 file 身份，download resolution 只接受明确 VersionId/FilesId 并生成受 broker origin/auth/header 限制的 descriptor，不在组件内拼 URL 或选择 latest。
  - provider 实现 live pagination、详情、type/tag、review、版本、ModCat 与 mod.io 依赖映射、URL parser、HTTP/envelope auth/not-found/rate-limit 错误。credential 仍是 opaque reference，`Origin`/`Referer` 兼容要求作为 download descriptor header 而非全局浏览器状态。
  - vNext 各 consumer 只经共享 registry 调用本 provider；隔离旧 UI 的版本组件保留到 R34，但其拼接逻辑不进入 vNext。

  验证证据：

  - `cargo test --workspace` -> 54 tests passed。ModCat 4 项 loopback 测试证明 DRG/RC request/result binding 分离、字符串 ModId 保留、同 Mod 的旧/新 VersionId 对应不同 FilesId、旧版本下载不漂移、跨来源依赖、恶意 host link、错误 game detail 与过期 bearer 都产生明确结果。
  - 独立 provider smoke（临时 example 已删除）通过真实 POST/broker 查询 Rogue Core fixture，输出 `requestBoundToRogueCore=true, remoteGameKey=\"rogue-core\", remoteModId=\"modcat-smoke\"`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R11 无代码阻塞。R00 没有 ModCat 授权测试账号，使用受控 HTTP 契约服务；最终 game descriptor 在 R20/R21 提供映射，R14/R15 消费 download descriptor。

- [x] **R12 — 迁移 Nexus 来源及浏览授权流程**
  - 前置：R09。
  - 目标：`src/apis/nexusmods`、search provider、FFI 中 nexus_webview、宿主 webview policy 边界 -> `modules/source-nexusmods`。
  - 动作：保留必要网页登录/nxm/file-id 语义；业务返回受限交互需求，宿主掌控实际窗口权限与导航边界。
  - 验收：授权正常/拒绝/关闭/过期、错误 file-id、部分搜索失败、headless INTERACTION_REQUIRED 后继续；不能绕过平台下载规则。
  - cutover：不再靠前端 CLI bridge 注入一次性 Promise 完成下载。

  执行记录（2026-04-12）：

  - 新增 `modules/source-nexusmods` 并接入共享 `SourceProvider`。GraphQL search 的 cursor 绑定 game/query/sort/page size，远端结果再次核对 Nexus domain；`data + errors` 保留成功项并返回结构化 `SourceWarning`，无 data 的错误不再伪装空列表。
  - REST detail/files/download-link 使用明确 domain、mod ID 和 file ID。稳定版本身份为 `file:<fileId>`；显式旧 file、错误 file、同名歧义版本均不会回退 latest。API key 仅以 broker `CredentialRef` 注入 Nexus API origin，下载 URL 必须落在 descriptor 声明的 CDN origin。
  - `runtime/broker` 新增 durable `browser-download` action：宿主逐次校验 navigation origin、callback origin 或明确的 `nxm` scheme，成功回调存为一次性 grant，消费后擦除受保护 URL。`runtime/jobs::reject_action` 和 host rejection 将拒绝/关闭转为 `Rejected` action 与带 `INTERACTION_REJECTED` 的失败 job。
  - premium/403 或无可用 link 时，headless provider 返回绑定 game/mod/file 的 `ActionRequired`，不合成或绕过平台 URL。继续执行时再次核对 action target 与捕获的 `nxm` game/mod/file、key、expiry；短期 key 只用于 broker 内 API exchange。错误 file、过期 key、恶意导航、重复消费均明确失败。
  - vNext Nexus 路径不调用 renderer Promise、CLI bridge 或 `nexus_webview` FFI。旧前端实现仅作为 R34 最终删除前的隔离 legacy 路径，不被新 provider 使用。

  验证证据：

  - `cargo test --workspace` -> 59 tests passed。Nexus 5 项 loopback 测试覆盖 game-bound search/cursor/sort、GraphQL 部分失败、API-key header、详情/版本、精确旧 file、错误 file、403 交互、受限 navigation、正常 `nxm` continuation、错误 callback file、拒绝/关闭、过期 key、一次性消费与无凭据 401。
  - 独立 headless smoke（临时 example 已删除）经真实 broker/loopback HTTP 走 files -> 403 -> durable action，输出 `{\"actionKind\":\"browser-download\",\"fileId\":\"10\",\"interactionRequired\":true,\"sourceId\":\"nexusmods\"}`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R12 无代码阻塞。R00 没有 Nexus Premium/真实网页登录测试账号，网络与授权边界按既定输入使用受控 HTTP/NXM 契约服务验证；R15/R23/R24/R32 消费 provider 与 action 的最终应用/UI/Agent/CLI 表面。

- [x] **R13 — 实现本地来源和不可变输入导入**
  - 前置：R01、R04。
  - 目标：`modules/source-local`；现有本地文件/Content/PAK/ZIP/JS/native 输入适配。
  - 动作：授权路径、目录扫描、内容摘要/快照、输入分类；拒绝链接逃逸、非法归档路径和资源上限；标注可执行内容风险。
  - 验收：合法小型输入、修改中的目录、同内容不同路径、非授权路径、恶意归档都产生正确结果；原输入不因校验失败被任意删除。
  - 禁止：未经检查 follow_links 或仅用字符串 startsWith 限制目录。

  执行记录（2026-04-12）：

  - 新增 `modules/source-local`。文件和目录输入只能消费宿主完成的 durable selection action；业务 API 不接受任意路径字符串。文件通过授权 file handle 直接流式发布到 content-addressed artifact，目录通过 opaque `AuthorizedDirectory` 进入有界快照。
  - `runtime/broker` 增加 directory selection grant 和 no-follow snapshot：逐项拒绝 symlink/特殊文件/越界 canonical path/非 UTF-8 或不可移植路径/大小写冲突，限制深度、条目、单文件与总字节；复制前后重扫 metadata 并复核每个源文件 digest，变化时返回 `InputChanged`。grant 消费后清除持久路径。
  - 目录按排序后的相对路径和字节生成确定性 `application/vnd.mintcat.directory-snapshot` artifact；根目录名称/原路径不进入 digest。因此同内容不同路径得到同一身份，源目录后续变化不会改写已发布 artifact。`ArtifactStore::publish_blob_reader` 支持从授权 handle 发布，不需要暴露源路径。
  - PAK、ZIP、unpacked `Content`、JS 和 native 输入获得明确分类。ZIP 在发布后的不可变副本上完整读取并校验 CRC/声明长度，拒绝 traversal、绝对/drive/ADS/backslash 路径、link/特殊文件、重复/大小写及 file-directory 冲突、深度/条目/单项/总展开量和压缩比越界；JS/native 数量形成显式 `requiresApproval` 风险。
  - 所有失败只留下可由 artifact GC 回收的未引用副本；不删除、移动或改写用户原输入。旧 renderer 的通用 fs/路径直传仍隔离在 legacy 路径，最终由 R23/R34 cutover 删除。

  验证证据：

  - `cargo test --workspace` -> 66 tests passed。local source 7 项测试覆盖授权 action/stale binding、错误扩展、合法 PAK、同内容不同路径、确定性 Content/JS/DLL 目录、源修改后新 digest、symlink 逃逸、目录条目上限、ZIP traversal/drive/ADS/大小写冲突/压缩炸弹、完整 ZIP 读取和 executable risk；失败后原文件/目录均仍存在。
  - 独立 smoke（临时 example 已删除）通过真实 host directory action、broker snapshot 和 artifact publication，输出 `sourceId=\"local\", inputKind=\"unpacked-directory\", entryCount=2, requiresApproval=true` 及稳定 SHA-256 digest。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R13 无代码阻塞。目录 artifact 使用版本化 MintCat snapshot 容器，R18/R19 消费其内容；R14 负责远端 transfer/发布和统一长期引用，R23 提供最终选择 UI。

- [x] **R14 — 实现共享 transfer 与内容发布**
  - 前置：R04、R05、R09。
  - 目标：下载执行器、artifact store、现有 `download/*`/`DownloadApi` 的可复用实现。
  - 动作：transfer/attempt/consumer 分离，固定文件身份、独立 part 路径、有效 validator 的续传、下载/验证/发布分阶段；按安全文件身份共享下载，完成后按 digest 去重。
  - 动作：进度事件只含 transfer 身份/seq/字节；取消 consumer 不影响其他消费者；verified handle 有持久引用保护。
  - 验收：两个 Profile 共用同文件，一个取消另一个继续；同 Mod 不同版本并发；文件变更不能拼接错误断点；100% 未验证不能标完成；掉电不暴露半文件。
  - 禁止：用 modId 或带过期 token 的 URL 当唯一内容身份，覆盖正在使用的 cachePath。

  执行记录（2026-04-12）：

  - 新增 `runtime/transfers`。Kernel SQLite 分表持久化 transfer、独立 attempt、consumer、artifact reference 与单调 progress event；每个 attempt 使用不可复用的随机 `.part`，transfer snapshot 和 grant 从不暴露 staging path。
  - 共享身份固定为 source/game/Mod/resolved version/remote file/byte variant 加验证契约；短期签名 URL、header 和 credential 仅在本次 broker 请求中存在。没有可信固定文件身份时使用 `AfterDigestOnly` 隔离 transfer，发布后由 `ArtifactStore` 的 SHA-256 路径自然去重。
  - `runtime/broker` 增加有 origin/redirect/credential/大小/取消/空闲超时边界的流式响应。断点只使用已同步字节数和 strong ETag/Last-Modified；`206` 必须同时匹配 validator 与 `Content-Range` 起点。服务端改为新 validator/完整 `200` 时先截断新 attempt，禁止拼接旧字节。
  - 状态严格经过 downloading -> verifying -> publishing -> succeeded。长度、来源 MD5、调用方格式前缀和最终 SHA-256 均在成功前检查；content artifact 原子发布后，artifact 与所有仍活跃 consumer 的持久引用在同一事务记录，随后才可取得 read grant。收满字节的事件仍是 downloading。
  - consumer 取消独立于物理传输；仅最后一个 consumer 离开才取消 broker I/O。启动恢复把 downloading/verifying/publishing attempt 标为 interrupted/queued，不把半文件当 artifact；只有 validator-bound checkpoint 可供后续携带新 descriptor 的 attempt 恢复。持久 digest 集合可直接作为 artifact GC 保留根。

  验证证据：

  - `cargo test --workspace` -> 73 tests passed。transfer 7 项确定性 loopback/调度测试覆盖两个 Profile 共享且单方取消、同 Mod 两版本同时在途、同 validator 精确续传、remote validator 改变后截断、100%/verify/publish/success 次序、进程中断恢复且 part 不可见、重启后的持久 consumer reference，以及不可靠身份仅在 digest 发布后去重。
  - 独立 smoke（临时 example 已删除）通过真实 loopback HTTP、broker stream、两个 consumer、单方 detach、校验、artifact 发布与剩余 consumer grant，输出 `sameTransfer=true, state=\"succeeded\"`，grant digest 与 `sha256:a152bebc42d4b428dd3a3a11e5b2123b49fe8407d9942848dc8d252a78234b53` 一致。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R14 无代码阻塞。R15 负责以 Library outbox/operation/fence 原子接管 durable artifact reference 并完成 cache-only/prepare-and-apply；R16/R23/R24/R34 完成 UI/Agent/CLI/legacy `DownloadApi` cutover。在 cutover 前旧 Tauri 下载路径仍与 vNext 隔离，不作为 transfer 身份或发布依据。

- [x] **R15 — 实现下载并应用 Profile 的完整事务用例**
  - 前置：R07、R08、R10、R11、R12、R13、R14。
  - 目标：替代 `ModUpdateService.updateModFile/batchDownloadModFiles` 的执行编排。
  - 动作：明确 cache-only 与 prepare-and-apply；固定版本输入；原子取得 Profile 内容锁并登记 outbox；全部文件验证后以 fence/revision/incarnation 条件原子应用整批。
  - 动作：源最新版本、用户选择、缓存版本、安装版本分开；超时/取消/恢复与迟到回调遵循专项协议；批量失败保留旧可用绑定。
  - 验收：完整执行专项第 12 节并发矩阵，特别是下载中切版本、删除重加、metadata 更新、取消/完成同时到达、多 Profile 多版本。
  - cutover：所有单个/批量/版本切换/添加后下载使用同一操作，不残留前端直接更新全局 currentVersion/cachePath 的后门。

  执行记录（2026-04-12）：

  - 新增 `runtime/downloads` 作为唯一编排器，连接 SourceProvider、NetworkBroker/TransferManager、ArtifactStore、durable Job/Action 和 Library owner。`DownloadCatalog` 固定 provider 模块摘要与显式 game binding；每项 descriptor 必须与 operation 中冻结的 source/game/Mod/version/file identity 完全一致。
  - Library schema v4 持久化 remote game key、待应用的 `VersionSelection` 与逐项 verified artifact。`mods.setVersion` 先解析 exact version/file 再登记 operation/outbox，不提前写 selection 或 `usedVersion`；相同幂等键在 Profile 已提交或已删除后仍返回原 operation。
  - `cache-only` 使用 operation 级资源和 lease，不依赖后续 Profile 存在；`prepare-and-apply` 原子取得 Profile-content lease。协调器允许逐项缓存，但只在所有 grant 齐全后进入 commit，以 fence/generation/revision/incarnation 和每项固定输入一次性切换整批 binding/selection；任一失败保留旧 binding 与 content revision。
  - SourceProvider 增加统一 begin/resume download action。交互 action、job checkpoint、operation plan 与 module-lock digest 持久关联；批准后恢复同一计划。取消先撤销 Library 提交权再 detach consumer，最后一个 consumer 才停止 broker I/O；共享 transfer 不受其他 Profile 取消影响。
  - 重启恢复 queued/checkpoint job，并通过新 generation/fence 继续 Library operation；若 Library commit 已完成而 job finish 响应前中断，则从持久 operation result 结束 unknown commit，不重复 apply。daemon 的 `mods.setVersion`、action approve 和 job cancel 已全部接入该协调器，旧的即时 version-intent 路径已删除。

  验证证据：

  - `cargo test --workspace` -> 81 tests passed。`runtime/downloads` 8 项确定性测试覆盖原子版本切换、Profile 删除后的 cache-only、多项中途失败保留旧 binding 与部分缓存、两个 Profile 共用 transfer、持久交互批准、取消后的迟到响应、checkpoint 新 fence 恢复，以及 Library 已提交/job 未结束的 unknown-commit 对账；R07/R14 测试继续覆盖编辑/删除/双客户端竞争、共享取消、不同版本、validator 变化与半文件恢复。
  - 独立 smoke（临时 example 已删除）通过真实 loopback HTTP、source descriptor、broker stream、transfer 校验/发布、Library apply 与 durable job，输出 `operationState=\"succeeded\", jobState=\"succeeded\", contentRevision=2, usedVersion=\"v2\"`，artifact digest 为 `sha256:11f28945d28bf6e5bac156734d8dc6c11a51bda568b5da1a4b6e529b4a96a089`。
  - `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 和 Windows target workspace clippy 通过；Windows 仅保留现有 GNU/MSVC helper warning。并将 transfer 掉电测试的固定 yield 轮询改为有超时的状态等待，避免并行 workspace 检查造成假阴性。

  限制/阻塞：R15 无后端代码阻塞。vNext 所有已暴露版本下载命令均走统一 operation；R16 把 renderer 的单项/批量/添加后下载入口迁到这些命令并删除旧 `CompleteModData` 进度覆盖，R24/R34 分别完成 Agent/CLI 和 legacy `DownloadApi` 退场。

- [x] **R16 — 重构下载期间的列表 UI 和能力提示**
  - 前置：R15。
  - 目标：`TreeViewItem.tsx`、TreeViewModel/HomeViewModel、拖拽/菜单/快捷键、事件 registry。
  - 动作：Library query cache 与 transfer overlay 分离；pending 版本与生效版本分开；统一 capability availability，明确 busy owner/查看任务/取消后编辑入口。
  - 动作：删除下载进度全量覆盖 CompleteModData、按 modId 的权威进度缓存和只在组件内生效的版本切换锁。
  - 验收：真实 UI 下载时尝试所有内容编辑入口应一致受限；仍可搜索、查看、切 Profile、编辑允许的名称；旧进度事件不覆盖新名字、版本或创建幽灵行。
  - 要求：UI disabled 只是体验，后端 R07 的拒绝路径必须仍存在。

  执行记录（2026-04-12）：

  - contract set 1.5 新增 `profiles.contentState`。Library 在一个 read transaction 返回 contentRevision、Profile-content lease owner/recovery 状态及活动 operation 固定项；daemon 关联 durable job 和当前 consumer 的 transferId/attemptId/seq/phase/state/byte overlay，初始 outbox 窗口也能以 operation 状态回退，不要求 renderer 保存内存任务表。
  - vNext desktop 新增独立 Library panel/model。`mods.list` query cache 只持有领域行，download overlay 只按 `profileModId`/incarnation 更新 pending version 和进度；contentRevision 变化才失效查询。旧 Profile 响应有 generation guard，轮询不重叠，overlay 不携带/覆盖 displayName、enabled、order 或 appliedVersion，也不创建不存在的行。
  - checkbox、批量启停、版本选择、drag/drop、action menu、Space/Delete/Alt+Arrow 快捷键全部消费同一 `writable` capability，并在能力尚未返回时默认关闭。busy/recovery 明示 owner operation、job 状态、查看详情和取消入口；搜索、Profile 切换、刷新和 shared rename 保持可用。所有 mutation 仍携带 expected revision/idempotency key 并由 R07 后端 gate 最终拒绝竞态。
  - download coordinator 发布 item 计数事件，并提供只读 active transfer snapshot；UI 显示 applied 与 pending 两条独立状态。交互批准后 job 立即回到 running progress，不再在真实下载期间残留 awaiting-approval。

  验证证据：

  - `cargo test --workspace` -> 81 tests passed；最终交互恢复和 gated cancellation/transfer-progress 路径另行定向通过。daemon IPC 测试断言 busy capability、owner operation、job、pending selection 与未推进 contentRevision；transfer 测试在真实受控 HTTP 响应中读取 attempt/seq/bytes 后再触发 cancel race。
  - `pnpm test` -> 8 files、24 tests passed；新增模型测试证明 transfer overlay 使用身份化进度且不能改写 Library 名称，并按后端 contentRevision 失效。`pnpm vnext:check`、contract generation check 和带显式 data-dir 的 desktop production build 通过，最终 bundle 含完整 Library surface。
  - 真实 Chromium smoke 使用生产页面和受控 native bridge 响应渲染 busy 下载：画面同时显示 `Applied: v1`、`Pending v2 · running · 50%`、owner operation 与 task；DOM 断言所有 content controls disabled，而 rename、search 与 Profile selector 可用，最新名称 `Renamed while download active` 未被 overlay 覆盖。
  - host/Windows `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 通过；Windows 仅保留现有 GNU/MSVC helper warning。

  限制/阻塞：R16 无 vNext 代码阻塞。旧 renderer 的 `CompleteModData`/`ModUpdateService` 路径仍只属于隔离 legacy 应用，不能调用 vNext operation；R23 完成完整 Workbench slot 集成，R34 删除这些 legacy 路径。

### 第三组：游戏打包安装的正确执行边界

- [x] **R17 — 实现游戏文件事务、journal 与 receipt**
  - 前置：R04、R05。
  - 目标：`runtime/file-transactions`；独立于具体游戏。
  - 动作：root 物理身份锁、grant/path 校验、同卷 staging、磁盘/进程检查、备份/替换/持久 journal、COMMITTED marker、所有权 receipt、恢复和取消安全点。
  - 动作：只删 receipt 自有文件；用户修改/外来文件保留并报告；处理两条路径指向同一目录、跨卷和被占用文件。
  - 验收：临时目录中逐步骤注入异常/结束进程，重启检查旧或新文件完整；未知状态进入 recovery-required，不盲写；取消与 commit 的结果真实。
  - 禁止：多文件 rename 假装天然原子、先删除旧安装、失败只返回 false。

  执行记录（2026-04-12）：

  - 新增 `runtime/file-transactions`，由 Kernel migration 独占 `file_transactions`/`install_receipts`。事务按物理 root 身份取得跨进程文件锁；Unix 使用 device/inode 合并 symlink alias，Windows 使用规范化目标路径。prepare 强制安全相对路径、无 symlink/junction ancestor、预期旧摘要、进程停止、可用空间和 root-local 同卷 staging，并只通过不可篡改 `ArtifactGrant::open_read` 流式复制与复核 digest/size。
  - apply 先持久化 PREPARED journal，再逐文件保留旧文件、替换 staged 工件、同步目录并持久化 sequence/state；完整 receipt 和 `COMMITTED` marker 先于 SQLite receipt 索引。marker 前失败按磁盘实际摘要回滚；marker 后索引失败明确返回 recovery-required，重启可从 root-local receipt 重建结果。外部修改、备份变化或 root 身份变化停止在 recovery-required，不盲写。
  - commit cancellation 在首个文件 mutation 前可取消；进入 apply 后延迟到真实提交/恢复结果，并在结果中显式标注。receipt 包含每个目标的 before/after digest、mode、owner、backup 与目录，以及 game fingerprint、Profile revision、planHash、module lock。uninstall 只处理 digest 仍匹配的 receipt-owned 文件；修改文件、缺失文件和外来文件分别保留/报告，不按目录名广泛删除。
  - `runtime/artifacts` 的 grant 保存不可见的 issued descriptor 与真实路径；调用方篡改公开 descriptor/access/path/digest 后不能再打开底层文件。workspace 已登记新 crate。

  验证证据：

  - `cargo test --workspace` -> 88 tests passed；其中 `runtime/file-transactions` 7 项覆盖完整 commit/receipt/uninstall、提交前与提交中取消、进程/路径/旧摘要/篡改 grant 拒绝、物理目录 alias 互斥、每个 filesystem mutation 后模拟进程终止并重启回滚、marker 已写但索引未提交的重建，以及外部修改进入 recovery-required。
  - 实际临时 example（验证后删除）完成 ArtifactStore publish/grant -> prepare -> commit -> receipt-owned uninstall，观察 `committed=true, restored=[\"owned.bin\"], foreignIntact=true`；正式目标恢复旧 bytes，外来文件保持原 bytes。
  - `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check` 通过；`cargo clippy -p mintcat-runtime-file-transactions --all-targets --target x86_64-pc-windows-gnu -- -D warnings` 通过。完整 Windows workspace gate 在本机分别被缺少 MSVC SDK C headers 和现有 Tauri GNU `resource.lib` 生成失败阻断，不是本 crate 的诊断。

  限制/阻塞：R17 无代码阻塞。具体游戏的真实进程枚举/独占加载文件探测由 R20/R21 的 `TargetProcessGuard` 提供；R17 已以同一拒绝/恢复 contract 验证 running target。R22 消费 DeployPlan、批准与 receipt，并负责无 receipt 旧安装的接管流程。

- [x] **R18 — 提取 Unreal PAK 引擎模块**
  - 前置：R04、R13。
  - 目标：`mintcat-integrator-core` 的共用 PAK/ZIP/unpacked/conflict/writer -> `modules/engine-unreal-pak` 与算法 crates。
  - 动作：提供 inspect/conflicts/build/verify 契约，流式大文件、压缩/codec 来源、输入输出 artifact handle；保留当前顺序语义和大小写归一规则。
  - 动作：把直接写游戏正式目录改为输出 staging 工件；游戏专有 patch 不进入通用 engine。
  - 验收：合法 fixture 的冲突获胜者、输出资源内容/索引/压缩和损坏输入处理；同输入和相同明确构建条件得到一致内容；验证目标平台 codec。
  - 限制：明确实际支持格式，不用 RC 存在就宣称通用 UE5.6 完整支持。

  执行记录（2026-04-12）：

  - 新增 `algorithms/unreal-pak`：独立封装 repak metadata reader、mount/entry 路径校验、base-PAK case reference、重复路径拒绝和 deterministic V11 writer。build codec 是明确枚举，仅允许 uncompressed/Zlib；inspection 保留实际 version/mount/encrypted/codecs。legacy raw-path API 被隔离在 feature adapter，不进入新 engine contract。
  - 新增 `modules/engine-unreal-pak`，唯一正式输入是不可篡改 `ArtifactGrant`，支持 PAK、恰含一个 PAK 的安全 ZIP 和 R13 directory-snapshot artifact；输出经 `ArtifactStore` 发布，不接收游戏正式目录。SDK 提取了 directory snapshot media type/magic，source-local 与 engine 使用同一 wire contract。
  - `inspect/conflicts/build/verify` 固定输入 identity/order/format/content prefix、reference casing、target 与 planHash 并生成 build-conditions digest。冲突和实际写入共享 first-wins winner map；manifest 记录每个 path 的 digest/size/winner/shadowed identities。entry 流入有界磁盘 spool，repak 连续 slice 仅 mmap 当前 entry，不把整份输入读入内存。
  - ZIP/snapshot/PAK 路径拒绝 traversal、absolute/drive/ADS、link/special file、大小写/parent-child collision、重复 output 和资源上限；输出发布前后都重读 index 和每项 bytes 验证。encrypted index 无 key contract，Oodle 无批准 codec artifact，build 明确拒绝；没有把 RC 存在解释成完整 UE5.6 支持。
  - legacy `mintcat-integrator-core` 的 common writer、ZIP、unpacked 和 conflict 实现已切到新 algorithm adapter；原实现删除。game-specific AssetRegistry/Blueprint patch 仍留在 DRG/RC，正式 vNext engine 不含游戏规则；其旧安装器对正式目录的写入由 R20/R21/R22 cutover，而不是混入本 engine。

  验证证据：

  - `cargo test --workspace` -> 94 tests passed；新 algorithm/engine 6 项覆盖 path/case、baseline PAK/ZIP、directory artifact、first-wins winner、输出资源 bytes/index/Zlib、两次相同构建 digest/manifest 一致、损坏/穿越输入、unsupported target 和 manifest 篡改。
  - nested legacy `cargo test -p mintcat-integrator-core` -> 7 tests passed，现有 baseline PAK/ZIP/directory fixture 与旧数据库 fixture 保持兼容。`cargo clippy --workspace --all-targets -- -D warnings`、双 workspace format check，以及 Unreal crates 的 `x86_64-pc-windows-gnu` all-target/all-feature clippy 通过。
  - 实际临时 example（验证后删除）通过 ArtifactStore grants 构建并再次验证 Zlib PAK，观察 `files=1, codec=[Zlib], winner=first, shadowed=[\"second\"]`，输出 digest `sha256:74872e89724ca53e31a4dc0f4ce0b6a00af44997fef207e70404ca6a0cc3f064`。

  限制/阻塞：R18 无代码阻塞。可写 target 目前明确为 PAK V11 + none/Zlib；Oodle 需后续已签名、许可明确且目标平台验证的 codec artifact。nested legacy workspace 的全量 `-D warnings` 仍被既有 UE4SS 文件布局、未用 DRG helper 和 uasset 算法的 37 项 lint 阻断；本卡迁移路径已通过 compile/test，相关源码分别在 R19/R20 cutover 时删除或迁移，不用 `allow` 掩盖。

- [x] **R19 — 提取 UE4SS/资源部署模块**
  - 前置：R18。
  - 目标：`common/ue4ss.rs`、音频/native/JS 布局与资源包 -> loader/resource 模块。
  - 动作：生成受限部署清单而非直接解压到真实游戏；版本化资源引用；路径与归档安全；把可执行资源纳入批准。
  - 验收：DRG/RC 设置差异正确；JS/DLL/音频输入只输出声明路径；卸载不删除第三方共用目录；资源可按游戏锁定不同版本。
  - cutover：移除 broad rm、源 ZIP 校验失败后任意删除和不受控全量解压路径。

  执行记录（2026-04-12）：

  - 新增 `modules/loader-ue4ss`。正式 API 仅消费不可篡改 `ArtifactGrant`，或由 `ArtifactStore` 已验签且持有 lease 的资源模块；返回 exact-file `DeploymentManifest`，从不接收游戏根目录。资源模块每个签名文件必须显式绑定一个 portable target，遗漏、重复、大小写 collision、父子 file collision、未签名 executable 标记都会拒绝。
  - `ResourceLock` 按 game plan 固定 resource ID/version/package digest/publisher/signing key；DRG 与 RC 可同时锁不同版本。可执行签名资源的 approval 同时绑定输出 artifact digest 和该 lock。RC 从签名模板确定性加入 UE 5.6、`HookProcessInternal=1`、`HookProcessLocalScriptFunction=1`；DRG 不注入 RC 覆盖且保留模板其他键。
  - JS planner 支持安全 ZIP 和共享 versioned directory snapshot，只发布唯一 `js/main.js` 根下的 exact files；其他 PAK/content 仅进入 `ignoredInputs`。native planner 只输出 `ue4ss/mods/<id>/main.dll` 并验证 MZ/PE/DLL header；audio planner 只接受 PAK 或恰含一个 PAK 的 ZIP，检查 index/禁用 game-state 内容并输出一个显式 game Paks target。三者分别绑定 script/native approval 或无代码 approval，不存在任意目录 extraction。
  - ZIP/snapshot 路径拒绝 traversal、absolute/drive/ADS、Windows reserved name、非 UTF-8、link/special file、大小写/parent-child collision；entry/input/expanded size、count 和 compression ratio 都有上限。SDK 的 directory-snapshot parser 提取为共享的精确长度/index contract，R18 engine 同步切到该 parser。
  - manifest 只有 artifact/相对文件/owner，没有目录删除操作；卸载交给 R17 receipt。legacy fallback 已停止损坏 ZIP 后删除源 artifact、删除不受控整份 JS ZIP helper，并停止递归删除共享 `ue4ss`/`mods` 目录；回归测试保留第三方目录和损坏源 bytes。DRG/RC 仍调用的 raw UE4SS resource ZIP adapter 只作为 R20/R21 cutover 输入，正式 loader contract 不暴露该路径，游戏迁移后删除。

  验证证据：

  - `cargo test --workspace` -> 102 tests passed；loader 7 项与 SDK/settings 相关测试覆盖签名资源完整绑定、跨游戏不同版本 lock、DRG/RC 设置差异、ZIP/snapshot JS 选择、PE DLL、单 PAK audio、approval、target collision、traversal/corrupt archive、未声明资源和 game-state PAK 拒绝。nested legacy `cargo test -p mintcat-integrator-core` -> 9 tests passed，新增损坏 ZIP 保留和共享目录保留回归。
  - `cargo clippy --workspace --all-targets -- -D warnings`、双 workspace format、以及 SDK/Unreal engine/UE4SS loader 的 `x86_64-pc-windows-gnu --all-targets -D warnings` 通过。nested legacy 全量 lint 仍只有 R18 已记录、将在 R20/R21/R34 删除的旧实现告警，不用 `allow` 掩盖。
  - 实际临时 example（验证后删除）完成 signed resource publish/verify/lease -> RC settings artifact -> directory-snapshot JS -> manifest merge，观察 `lock=resource.ue4ss@3.1.0`、targets 仅 `UE4SS.dll,ue4ss/mods/smoke-js/js/main.js,ue4ss/UE4SS-settings.ini`、`approvals=2`、`ignored=[\"mod.pak\"]`。

  限制/阻塞：R19 无代码阻塞。正式资源必须是受信签名 module package，不接受未签名“资源 ZIP”；legacy DRG/RC 的 raw-path 调用点必须分别由 R20/R21 替换，R34 再删除兼容 workspace。该过渡路径不是新 contract，也不进入 daemon/CLI。

- [x] **R20 — 形成独立 DRG 游戏安装插件**
  - 前置：R18、R19。
  - 目标：`drg/installation.rs`、pak_integrator/game_pak_patch 等 -> `modules/game-drg`。
  - 动作：独立 manifest/version/worker；Steam/支持的其他布局识别、运行检测、专有 patch、BuildPlan/DeployPlan、来源 game-key、settings schema 和资源依赖。
  - 动作：压缩/UE4SS 等打包设置按 gameId 持久化并写入计划快照，不使用全局设置覆盖 RC；未知布局明确不支持。
  - 验收：DRG 代表输入输出内容一致，先前安装保持可恢复；改变 DRG 设置不改 RC；单独构建包而不重编译/发布 RC。
  - 禁止：宿主 GameKind 分支或在通用 CLI 内硬编码 DRG 安装步骤。

  执行记录（2026-04-12）：

  - 新增共享 `modules/game/api` 和独立 `modules/game-drg` crate/worker。descriptor 明确拥有 Steam/Xbox 主 PAK、输出、目标 triple、精确进程名、mod.io/Nexus/ModCat/local game-key、signed DRG patch 与可选 UE4SS 资源依赖；未知 variant/PAK 名和缺少 AssetRegistry 的 build 均明确失败。
  - `GameSettingsSnapshot` 将 compression 与 UE4SS 选择绑定 `game:drg`、schema revision 和 digest；BuildPlan 同时锁定安装 fingerprint、Profile revision、module/resource digest、输入顺序与输出。RC snapshot 不能进入 DRG plan。pack-only 复用同一 builder 但不能产生 DeployPlan；正式部署必须含 signed patch lock、每文件 precondition，hook DLL 自动要求 native-code approval。
  - builder 只消费不可篡改 artifact grants，在隔离 staging 构造临时 game tree，经 DRG compatibility patch/build 后逐项重读 PAK index 并发布 artifact，从不写用户安装根。patch ZIP 仅允许声明的 hook 和 `Paks/` 内容，限制 path/type/count/entry/expanded/compression ratio，并要求 MI_SpawnMods asset pair。旧安装检测只接管确切已知输出；音频命名推断与共享第三方 UE4SS 目录保持 foreign。
  - 初始临时 feature 桥已在 2026-10-01 删除：DRG 算法位于 `modules/game-drg/src/integrator`，RC 算法与专属 UE4SS 设置位于 `modules/game-rogue-core/src/integrator`；共享 core 只保留 common/uasset/progress/validation。两款插件分别单独构建，正常依赖图互不包含；没有旧 game 导出、builder feature 或 `GameKind` 宿主分支。

  验证证据：

  - `cargo test -p mintcat-game-drg` -> 5 tests passed；合成、结构有效的 base PAK 与代表 Mod PAK 实际生成 `FSD-WindowsNoEditor_Mods.pak`，重读验证 Mod payload 和 AssetRegistry；同时覆盖两变体发现/验证、精确运行进程、settings 隔离、DeployPlan、patch traversal 和非破坏接管。
  - `cargo test --workspace` -> 121 tests passed；nested compatibility core -> 8 tests passed。`cargo clippy -p mintcat-game-drg --all-targets -- -D warnings`、双 workspace format，以及 game-drg 的 `x86_64-pc-windows-gnu --all-targets -D warnings` 通过。
  - 实际 standalone worker smoke 完成 framed hello -> `mintcat.game-adapter/describe` -> shutdown，观察 `moduleId=mintcat.game.drg`、`gameId=game:drg`、variants=`steam,xbox`、drained=true。该二进制由 `cargo build -p mintcat-game-drg --bin mintcat-game-drg-worker` 单独生成，未构建/发布 RC。
  - 2026-10-01 源码拆分与 worker `@2` 切换：受影响算法/engine/install/file-transaction/supervisor 回归 51 项、daemon lib 5 项、显式 `--features fixtures` 的 IPC lifecycle 4 项通过。真实签名 DRG/RC worker 经独立 Unreal provider 完成 prepare/build，验证 first-wins 冲突与 PAK payload、UE4SS 设置/JS bytes 的 grant 发布、脚本审批、关闭 loader 的拒绝和 pack 不写游戏根；缺少已声明 engine 拒绝，未声明 engine 的受控 manifest fixture 不隐式绑定。证据：`/tmp/mintcat-game-algorithm-split-proof.3pOiZy/worker-evidence.json`。临时 smoke 源码验证后删除；不把合法合成 PAK/INI/JS fixture 冒充授权 Windows 游戏安装。

  限制/阻塞：R20 无代码阻塞。仓库不含可提交的版权游戏 PAK；内容一致性使用合法合成 AssetRegistry/PAK fixture，真实版本矩阵仍需发布前在授权 Windows 安装上验证。正式 install 必须提供受信签名 patch package；测试不会用假签名绕过。

- [x] **R21 — 形成独立 Rogue Core 游戏安装插件**
  - 前置：R18、R19。
  - 目标：`drgrc/installation.rs`、pak_integrator 等 -> `modules/game-rogue-core`。
  - 动作：独立 manifest/version/worker、正式版/Playtest 发现、正确进程/路径检测、版本兼容声明、RC 特有构建规则和资源依赖；设置与 DRG 隔离。
  - 验收：RC 支持的输入实际可构建/验证；不支持的解析明确报错而非静默假成功；RC 缺失 Mod 清理和启动检查相关场景得到针对性验证记录。
  - cutover：消除“未知游戏默认 DRG”和全局固定 FSD.exe 的假设；不得复制整套通用下载/数据库代码进入游戏插件。

  执行记录（2026-04-12）：

  - 新增独立 `modules/game-rogue-core` crate/worker。descriptor 分别声明 Steam release `2605790` 与 Playtest `2860770`、共同但显式的 `RogueCore-Windows.pak` layout、`RogueCore-Win64-Shipping.exe`/`RogueCore.exe`、Nexus `deeprockgalacticroguecore`、ModCat `drgrc`、local game-key、UE 5.6 能力、signed RC patch 与可选 UE4SS 资源；不虚构 RC mod.io binding。
  - 安装验证只接受 unencrypted PAK V11，必须实际读取并解析 `RogueCore/AssetRegistry.bin` 后才生成完整 SHA-256 fingerprint。BuildPlan 锁 installation/Profile/module/resource/input order 与 `game:rogue-core` settings；DRG snapshot 被拒绝。builder 在隔离 staging 调用 RC compatibility algorithm、由 Unreal engine 重读输出并发布 artifact；pack-only 可独立使用，DeployPlan 必须有 signed RC patch lock。
  - RC patch archive 只允许 portable `Paks/` payload 并执行 type/count/entry/expanded/compression-ratio 限制。零输入实际重建干净 mod PAK，不携带上次已移除 Mod；旧安装接管仍只列确切输出并保留音频推断/第三方 loader 内容。
  - legacy `GameKind::from_game_pak_path` 与 conflict analysis 改为 `Result`/明确 game name，未知 PAK/name 不再默认 DRG，损坏输入不再打印后假成功。旧前端启动检查根据已选主 PAK使用 DRG 或 RC 精确 executable list，不再全局固定 `FSD.exe`。

  验证证据：

  - `cargo test -p mintcat-game-rogue-core` -> 6 tests passed：release/Playtest discovery、精确进程、受支持代表输入实际构建/重读、零输入移除缺失 Mod、不支持 AssetRegistry 明确失败、settings/deploy scope、patch traversal 与接管均覆盖。
  - `cargo test --workspace` -> 127 tests passed；root workspace `cargo clippy --workspace --all-targets -- -D warnings` 通过；RC package 的 native 与 `x86_64-pc-windows-gnu` all-target `-D warnings` 均通过；双 workspace format 与 `pnpm exec tsc --noEmit` 通过。legacy core 增加明确 game conflict 回归；legacy FFI 剩余 lint 是 R25/R34 的既有 proxy/unsafe export 清理，不以 `allow` 掩盖。
  - 实际 standalone worker smoke 完成 framed hello -> describe -> shutdown，观察 `moduleId=mintcat.game.rogue-core`、`gameId=game:rogue-core`、variants=`steam-release,steam-playtest`、drained=true；RC worker 可单独 build，不编译/发布 DRG worker。

  限制/阻塞：R21 无代码阻塞。仓库无版权 RC PAK，版本验证使用结构有效合成 V11/AssetRegistry fixture；授权 Windows release/Playtest 的真实 build fingerprints 与 signed production RC resource 仍是发布验收输入，不以宽松 fallback 代替。

- [ ] **R22 — 实现统一安装、打包、卸载与接管用例**
  - 前置：R08、R15、R17、R20、R21。
  - 目标：`modules/install`；替代 `ModInstallTask`、`IntegrateApi`、冲突/更新前置任务中的业务执行。
  - 动作：Profile snapshot -> 固定下载/模块闭包 -> 构建 -> 验证 -> 不可变 DeployPlan -> 批准 -> journal commit -> receipt；pack 复用同一 BuildPlan，不写游戏。
  - 动作：安装前核对目标未运行、内容/顺序/revision；旧无 receipt 安装先识别备份和接管；安装结果不依赖全局 success/error 事件。
  - 验收：无 GUI 分别完成 DRG/RC 的测试安装、pack、uninstall；输出摘要/获胜规则正确；关窗/取消/输入变动/失败不破坏旧安装；同物理目录互斥。
  - cutover：删除“先卸载再构建”、无 jobId 的完成订阅和宿主安装 DTO 的两游戏特判。

### 第四组：前端框架和剩余业务模块

- [x] **R23 — 实现插槽式 Workbench 与 UI 模块生命周期**
  - 前置：R16、R22。
  - 目标：`App.tsx`、`main.tsx`、`MenuBar.tsx`、IoCRegistration -> workbench/UI SDK/contributions。
  - 动作：页面/导航/命令/设置/游戏注册、React/UI kit 单实例、签名本地 ESM/chunk/CSS、Disposable、query/session state、错误边界和独立 rescue UI。
  - 动作：将 Library/Search/游戏设置/安装任务界面按所属模块迁入，禁跨模块 ViewModel/DAO import；设置 scope 明确到 gameId。
  - 验收：实际界面新增/停用模块，路由菜单随贡献变化；坏页不让框架白屏；关闭 UI 业务继续；没有编译期硬编码两个游戏的菜单。
  - 权限：迁移完成的页面不再依赖通用 fs/sql/shell；最终撤销剩余权限由 R34 验证。

- [x] **R24 — 迁移 Agent 功能与统一工具描述**
  - 前置：R23、R08、R15、R22。
  - 目标：AgentService/AgentToolRegistry/ModAgentToolService/AgentPanel -> `modules/feature-agent`。
  - 动作：LLM 会话与工具 adapter 模块化；从 CommandSpec 生成工具，业务执行只走 Application API；对话状态和 action/job 状态分开。
  - 动作：审批权限独立于“模型能调用第二条 confirm 命令”；令牌和 prompt 参数脱敏，保持有效 read/mutation 分类。
  - 验收：真实 UI/API 场景中模型建议不能绕过批准；UI/CLI/Agent 同一请求得到相同目标和权限结果；对话卸载不丢 task/action。
  - cutover：删除 ModAgentToolService 中被迁走的业务执行和前端内存审批权威源，不保留两份工具业务。

  实证（隔离签名基线）：`MINTCAT_AGENT_BASELINE=/tmp/mintcat-native-final-baseline MINTCAT_RUNTIME_CONFIG=/tmp/mintcat-native-final-baseline/runtime.json node /tmp/mintcat-r24-agent-proof.ptPRn3/prove-agent.mjs` 完整通过，未跳过错误/job 契约。实际 native Agent + 受控 HTTP provider 验证 create replay、断连后 durable turn、Library 工具变更、模型不可批准人工 action、错误 Profile 目标拒绝、cancel、401 脱敏、resume/delete 以及对话卸载后 task/action 保留。证据 `/tmp/mintcat-r24-agent-rerun.3e1EBI/evidence.json`；不计生产 LLM 凭据或 GUI 视觉验收。


- **R25 — Proxy/加速实验：已取消继续产品接入**
  - 状态：2026-10-01 按用户要求停止，不再安排系统证书、hosts、高权限进程和再分发许可验收来阻塞当前路线。
  - 保留：`modules/feature-proxy` 的真实实验实现、独立审批、长期 service lease 和原版本 stop 行为；不删除或伪造既有普通服务证据。
  - 当前范围：原库 UI 验证的签名基线不包含 `mintcat.feature.proxy`。取消不等于系统操作已验收，也不授予再分发许可；未来恢复实验需单独明确授权。

  实证：独立 native service smoke 已运行 ready/health/stop、撤回后原版本 stop、禁止撤回版本重启和失败清理；canonical Application 完整链 `/tmp/mintcat-proxy-application.aK8GEy/evidence.json` 又验证独立 CA 批准、真实 CA 校验 TLS 向 owned HTTP 上游转发、停用 UI 仍保持同一 service identity、stop 关闭监听、占用端口失败后无运行服务。离线规则为合法受控 fixture，不修改系统 trust/hosts。

  历史未验收项：系统 trust/hosts、高权限目标环境和未声明 LICENSE 的再分发边界仍未验证，仅在明确恢复实验时重新评估，不再列为 R32–R35 的前提。

- [x] **R26 — 迁移备份、恢复和配置导入边界**
  - 前置：R06、R07、R09。
  - 目标：CloudBackupApi、配置迁移和相关 UI -> `modules/feature-backup` 与数据 owner snapshot API。
  - 动作：一致导出各 owner 状态、schema/version 元数据、凭据引用规则；恢复时取得维护门禁，核对正在运行的下载/文件事务/模块版本。
  - 验收：副本上备份-恢复保留 Profile/顺序/版本；有活跃任务时拒绝或安全排空；缺失模块/新 schema 不损坏数据；UI 不直接复制或覆盖业务库。
  - 禁止：恢复全库绕过 R07，或把旧快照覆盖已发生的新写入当自动 rollback。

  实证：`python3 /tmp/mintcat-r26-proof-driver.py /tmp/mintcat-r26-canonical-final-proof /tmp/mintcat-native-canonical-baseline/runtime.json` -> 162 次单 envelope API 响应、322.19s，format2 Library schema5 + Agent/Proxy schema1 真实 native 多 owner 往返通过。实际持有 Agent HTTP turn 时 export 拒绝且无输出；非空、缺 owner/module、新 schema、伪造 nullable-operationId outbox、空 model/target 均拒绝且快照不变；只杀 owned Library worker/daemon 后，持久 maintenance 阻止新写入，显式 recover 清 marker，旧 job 失败且不重放，独立新批准重试成功。实际 source/restored RC Profile 页完全相等，旧 `rc` 标识的 games/profiles/mods/installations/hints/settings 已迁为 canonical `game:rogue-core`，source remote namespace 原样保留。证据 `/tmp/mintcat-r26-canonical-final-proof/summary.json`、`cli-proof.jsonl`、`artifact://1159`；取代前一版 baseline2 的导入记录。

  范围：一致逻辑 owner 快照和逐 owner commit，不是跨 owner 事务或整机备份；不移植 Kernel job/action/批准、执行 outbox/receipt/lease、凭据/vault、安装 journal/receipt、Proxy CA 私钥/hosts/controller、游戏文件与缓存 blob。无生产云端/OAuth 输入，未宣称云部署或 UI 视觉通过。


### 第五组：独立发布与安全热更新

- [x] **R27 — 实现模块打包、签名与 scope 发布元数据**
  - 前置：R04。
  - 目标：`tools/release`、module manifest/文件清单、target 工件、签名元数据和 ScopeReleaseSet。
  - 动作：模块独立版本、不可变 digest 包、完整 target triple、schema/契约/权限约束、TUF 风格真实签名/轮换/防回滚；发布时生成各 scope 精确依赖闭包。
  - 动作：准备隔离本地发布仓库用于演练；游戏各自 channel/sequence/灰度，公共 metadata 更新不要求其他游戏发包。
  - 验收：独立打包 DRG/RC/纯 UI/资源/native 包；真实验签拒绝篡改/错误长度/旧元数据；包身份相同字节不同的上传被拒绝。
  - 边界：不覆盖线上 OSS 指针；生产信任根/密钥权限缺失要报告，不填假签名或放开 release 验证。

  执行记录（2026-09-30）：

  - 已对独立 Library/engine/DRG/RC/Agent/backup/Proxy native 包和纯 UI/资源组件实际签名、compose、verify、publish 到隔离本地仓库；完整 9 包基线的 platform、DRG、RC 三个 scope 实际激活至 `confirmed`。
  - `cargo test --workspace` 本轮整合版本通过 211 项，包含包/metadata 的真实签名、篡改、长度、角色/防回滚与 scope 契约回归；其后发现的 native 首次安装与 CLI 契约缺陷另行修复验证，不用旧测试结果覆盖后续代码。
  - 打包后的独立 CLI 在无 runtime/config/daemon 环境变量且 `PATH=/nonexistent` 时找到 sibling daemon 与签名 Resources，实际返回 9 个启用模块。此项只证明本地测试根的离线基线，不代表生产签名或各 Windows 发行验收。


- [x] **R28 — 实现按 scope 的更新准备、激活、恢复与 GC**
  - 前置：R27、R05、R07、R17。
  - 目标：`runtime/updates`，替代 frontend/plugins/proxy/hot-update-store 多套 active 状态。
  - 动作：metadata verify -> resolve -> download -> stage -> safe point -> prepare -> scoped CAS activation -> probation -> confirmed；持久 pending/失败记录。
  - 动作：按 expectedScopeRevision 合入最新有效环境，路由按 scope/generation 选择 provider；任务/服务/UI/rollback leases 参与 GC；单 scope 失败局部回退。
  - 动作：单写者 schema 维护、真实 safe mode/rescue、离线已确认启动、revoke 的新任务禁止与安全收尾。
  - 验收：签名/契约/缺包/磁盘满/启动失败/中途退出；两个 scope 并行激活不互相覆盖；旧任务依赖经历多次升级仍保留。
  - cutover：更新判断/下载/激活只由本状态机执行，前端 StartupUpdateCheckTask 不再各自升级组件。

  实证（隔离本地）：完整签名 9 包 baseline 在同一真实 daemon 运行中完成 UI authority/session 的启停与 stale 请求拒绝、逐游戏设置隔离、DRG adapter/engine 0.2 与 RC engine 0.1 并存、RC 自身升级及 DRG 回退后保留 RC 0.2。修复 scope `confirmed` 位未参与 source/game generation 缓存键；新 job 的闭包实际切换到新 digest，回退只切对应 scope。记录 `/tmp/mintcat-native-gc-final-proof/native-reachable-evidence.json`、`artifact://1216`；同 native payload 使用不同真实签名版本元数据，不能代替游戏资源更新或真实游戏兼容性验收。

  GC 实证：先复现 GC 删除 Library 缓存输入后 `mods.pack` 失败，再合并 Library/transfer 与未过期 prepared、applying、active receipt 的 module/input/deploy 引用。真实 GC 保留 RC 输入 blob，两个 native adapter 随后重新打包，闭包、产物 bytes/hash 与 GC 前完全一致；过期未应用计划可回收，active receipt 过期后保留直到 uninstall。证据 `/tmp/mintcat-native-gc-final-proof/native-post-gc-evidence.json`；现有行为套件覆盖并行 scoped CAS、旧任务/服务/UI/rollback 租约、坏签名、启动失败及中断恢复。

  I/O 实证：真实 64 MiB APFS native staging 返回 `ENOSPC`（error 28），operation=`rejected`、`quarantined=false`、active scopes 不变；7,553 ms 内 executor timer 触发 7,546 次。修复包装在 `ArtifactError::Io` 内的临时 I/O 被误判为坏签名包；故障修复后同一 signed release 重试通过。观测输出已记录于 `/tmp/mintcat-native-gc-final-proof/native-enospc-evidence.json`，回归在 `runtime/updates/tests/scoped_updates.rs`；生产 trust-root 与目标 Windows 不计入本项。

- [x] **R29 — 完成 UI 热切换与有状态模块升级闭环**
  - 前置：R23、R28。
  - 目标：UI loader/generation barrier、state migration、rescue 与客户端握手。
  - 动作：准备 registry、保存可迁移 UI state、脏编辑处置、dispose/remount/ready；共享 React runtime 变化则重建 WebView。
  - 动作：无 GUI 不偷偷启动隐藏窗口；有状态模块 drain/一致备份/迁移/只读 health/开放写入，严格定义可回退时点。
  - 验收：新页挂载失败可恢复，jobs 不丢；旧 chunk 不被删；已产生新数据后不会借 rollback 静默恢复旧 DB；客户端不能混用不兼容代际。
  - 禁止：仅热更新 URL 或重启页面就宣称完整模块热替换。

- [ ] **R30 — 验证每个游戏插件独立更新的硬约束**
  - 前置：R20、R21、R22、R28、R29。
  - 目标：game scope 目录/UI/API、依赖绑定与真实打包/安装执行路径。
  - 动作：演练只发布 DRG adapter、只发布 DRG 资源、DRG 使用 engine v2 而 RC 保持 v1、逐游戏灰度/固定版本/暂停/回退。
  - 动作：DRG 旧安装 job 运行时准备/激活新版本；RC 同时工作；在 RC 已完成另一次更新后回退 DRG。
  - 验收：主程序/CLI/RC 包不重发；RC provider/资源和已提交更新不被覆盖；旧 DRG job 全闭包不变，新 job 用新闭包；无关游戏可用性不受坏 DRG 包影响。
  - 完成证据：记录每个 scope 实际 digest、任务产物/receipt 和发布差异；不能只断言两个 module ID 不同。

  可达证据：`/tmp/mintcat-native-gc-final-proof/native-reachable-evidence.json` 记录实际签名 DRG engine v2 / RC v1、RC 独立更新、DRG 回退不覆盖 RC 新版本、旧/新 native pack 闭包和产物 hash、GC 后再次打包。资源独立发布、实际 install receipt 与旧安装任务并行仍依赖缺失的签名 patch/resource 输入；不把 pack-only 演练当完整 R30。

- [x] **R31 — 实现一套模块开发与发布工具工作流**
  - 前置：R27、R04、R23。
  - 目标：`tools/module-dev` 与 pnpm workspace 命令。
  - 动作：实现主设计中的 module new/dev/check/pack、release compose/verify/publish；隔离 dev store、测试签名、增量构建和按协议排空换代。
  - 动作：支持单模块及其必要 UI/后端组合调试，显示真实依赖/错误/版本/进程；生产包拒绝 dev origin 和未签名覆盖。
  - 验收：从新建模块到本地运行、构建包、签名目录演练一遍；无需手动复制 DLL、编辑 active JSON 或管理 endpoint 文件。
  - 限制：新模块模板可以是可运行示例，但不得用模板中的假业务实现作为已有游戏交付。
  - 实证：运行 `node tools/module-dev/smoke.mjs --bin target/debug/mintcat-module --key tools/module-dev/fixtures/TEST-ONLY-ed25519.seed --target aarch64-apple-darwin --root /tmp/mintcat-module-proof-drain-fixed --cli target/debug/mintcat --daemon target/debug/mintcatd`；实际新建、增量编译、签名 worker/UI host、framed invoke/drain、check/pack、精确 provider binding 的 compose/verify/publish、拒绝损坏工件和错误 target、CLI stage/re-verify 均成功。运行记录：`artifact://894`。本地根显式隔离，不是生产信任根；浏览器/native UI 和 Windows 发行另行验收。

### 第六组：CLI、发行与最终完整切换

- [ ] **R32 — 完成面向人和自动化的产品 CLI**
  - 前置：R08、R15、R22、R24、R26、R28。
  - 目标：从当前 `mintcat-cli` 迁移 argv/JSON 等可复用代码到 `apps/cli` 的 `mintcat` 产品入口。
  - 动作：实现 games/profiles/mods/install/uninstall/jobs/actions/modules/api 的真实领域命令、参数帮助、分页、稳定 JSON/退出码、输入文件/stdin、等待/事件流和完成提示；只调用 Application API。
  - 动作：按 scope 更新/回滚游戏插件；非交互批准/--yes 只作用于已获授权范围；确认与执行结果可查询；支持声明的数据目录发现，不依赖 GUI-ready。
  - 验收：桌面未启动时完成两游戏相关查询、列表变更、下载、打包和测试安装；超时重连能找回 job；Nexus 需要交互时准确返回；stdout 可稳定被脚本解析。
  - cutover：移除旧 `status/tools/call/confirm/reject` bridge 协议专用入口及前端转发依赖，不保留永久 aliases/fallback；更新机器客户端的契约说明。

- [ ] **R33 — 统一 standard/portable/fixed-WebView/独立 CLI 发行**
  - 前置：R20、R21、R27、R28、R31、R32。
  - 目标：现有 build-release/build-portable/build-fixed-webview2/package-*、Tauri bundle/resources/externalBin 配置。
  - 动作：全部消费者使用同一经过验证的 platform baseline 和各游戏 scope lock，包含兼容的 CLI/daemon/GUI/资源/codec；明确目标 triple 和签名/notarization 条件。
  - 动作：覆盖 Windows 常规安装、portable 和 fixed WebView2 的独立入口；修正新旧 runtime 文件名/target 架构猜测；其他平台只声明实际验证过的能力。
  - 验收：在干净目标环境构建并启动各产物，离线 baseline 可工作；CLI 可被找到且不依赖开发机 Cargo/pnpm；更新一游戏不要求重新发 installer。
  - cutover：消费者迁完后删除重复旧打包脚本/别名；没有验证的平台明确不支持，不伪造产物矩阵。

- [ ] **R34 — 完成生产数据交接准备与旧执行路径删除**
  - 前置：R23、R24、R26、R29、R30、R33。
  - 目标：旧前端 SQL/任务/CLI bridge、DLL FFI/loader、散落游戏分支、旧更新状态和全部文档。
  - 动作：在真实旧数据的授权副本完成一致备份/迁移/校验/恢复演练；切换唯一新写者；旧 installedHash 经接管而非伪造 receipt。
  - 动作：删除 AgentCliBridge/AgentCliService 的 renderer 执行、agent-cli-request/ready/respond、已废弃 `backend_invoke`/FFI/libloading、前端业务任务执行器、重复 API/aliases；撤销 SQL/通用 fs/shell 权限并收紧 CSP。
  - 动作：更新 README、CLI/热更新/分层/安装顺序/下载/事件/启动相关设计、ADR 与 ChangeLog；旧实现描述明确变更，不让新旧架构文档同时充当现状。
  - 验收：不存在仍可写旧库或走旧安装器的消费者；打开旧配置可迁移，错误可恢复；用户无关工作区变更未丢失。
  - 限制：本卡不授权实际生产发布或覆盖用户唯一数据；线上执行需要明确授权，软件和流程先在可恢复环境证明。

  可达证据：新入口已经删除 renderer SQL/业务任务/CLI bridge、FFI/libloading 和旧发行 aliases，所有业务由 canonical Application/native owner 处理；R26 的合成导入之外，本轮已在原数据库的一致只读副本完成真实游戏数据导入，2 游戏、2 配置、232 Mod、84 配置关联、303 版本、17 设置，未迁移凭据，原库与 WAL 哈希未变。生产覆盖、真实凭据迁移和完整发行切换仍未授权，本卡不勾选完成。

- [ ] **R35 — 执行最终端到端验收并交付可复核记录**
  - 前置：R34，且 R00–R33 的当前范围没有未解决行为缺口；已明确取消的 R25 不属于验收依赖。
  - 动作：执行主设计第 17 节、下载专项第 12 节、独立游戏更新 R30 的完整验收；跑受影响的现有行为套件和真实 UI/CLI smoke。
  - 动作：在目标平台实际验证游戏文件锁/恢复、native worker、签名工件、发行启动；记录无权限/无输入而未执行的场景，不编造通过。
  - 验收：GUI/CLI/Agent 一套业务；下载编辑无数据竞态；两游戏单独升级/回滚；关闭 UI 长任务继续；坏包/中断/数据迁移失败均有可解释恢复；没有仍可触发的旧执行链。
  - 交付：实际改动范围、运行命令与输出摘要、安装/更新/回退演练记录、平台能力和剩余外部阻塞。所有卡片证据齐全才可宣称重构完成。

## D. 不能遗漏的用户要求与对应任务

| 用户要求 | 实现任务 | 必须看到的结果 |
| --- | --- | --- |
| 细粒度模块覆盖前端、后端、游戏安装打包 | R01/R04/R18–R26 | 真实独立包/契约，不是只分文件夹 |
| 主界面只是框架 | R23/R29/R34 | contribution 装配，无业务 SQL/安装权威逻辑 |
| 模块从服务端热更新替换 | R27–R29/R33 | 可信下载、scope 原子激活、恢复和真实生效 |
| CLI 不方便，需要重构 | R03/R05/R08/R32 | 无 GUI 业务、领域命令、任务查询、可用输出 |
| 下载期间更新数据和列表编辑冲突 | R07/R14–R16 | 原子门禁、fence、条件提交、分离 UI overlay |
| 各游戏安装插件拆开更新 | R20/R21/R27/R28/R30 | DRG/RC 独立版本、依赖、channel 与回滚 |
| 后续 Agent 按 TODO 完成 | 全部卡片及执行记录 | 每卡依赖、范围、动作、验证和删除条件明确 |
| 视觉与原 UI 基本一致（被后续参考图改写） | R23 与各 UI 模块 | 原保留方案已实现；后续以用户深/浅毛玻璃图为最新视觉目标，不继续旧视觉验收 |
| 复刻深色参考图 | R23 与各 UI 模块 | 172px 文字侧栏、72px 工具栏、紧凑分组表格、粉色操作、深色毛玻璃；实际渲染，不写死截图 Mods |
| 复刻浅色参考图 | R23 与各 UI 模块 | 与深色一致的几何/功能，冰蓝浅色毛玻璃与深色文字；真实来源/任务/配置数据 |
| RC 清理缺失 Mod | R08/R21/R35 | 只移除目标 Profile 关联、保留共享 metadata、零输入 native pack |
| 打包设置按游戏独立 | R08/R20/R21/R35 | 更新一个 game scope 后另一个 settings 快照完全不变 |
| 启动检查用实际游戏可执行文件 | R20/R21/R35 | 当前声明名称进程阻止该游戏，另一游戏仍成功，停止后恢复 |

## E. 当前交接点

进度更新：2026-10-01。完整路线共 **36 项（R00–R35）**；当前 **29 项已验收、1 项已取消继续接入（R25）、6 项未完成**。R23/R29 的最终签名原生生命周期和同页 live 草稿热切换已通过；R22/R30/R32–R35 的剩余目标/输入前提在下表明确记录，不宣称整体重构完成。本轮改为只验证窗口/内容本身、系统原生三色按钮和原库真实游戏数据，不再复现参考图的桌面背景或 HTML 窗口按钮。

### E.1 已验收范围与证据边界

- R00–R21：唯一契约、独立 daemon、Library 单写者、job/action、内容门禁、broker/来源/transfer、文件 journal、Unreal PAK 与独立游戏 worker。游戏输入为合法合成 fixture，不等同真实 DRG/RC 版本矩阵。
- R24：`/tmp/mintcat-r24-agent-rerun.3e1EBI/evidence.json`，真实 native Agent、受控 HTTP 模型、权限/审批、断连/取消/恢复，不计生产模型账号。
- R26：`/tmp/mintcat-r26-canonical-final-proof/summary.json`，162 次单 envelope API、多 owner 导出/恢复、维护门禁、崩溃后不重放和 canonical legacy 导入。
- R27/R31：实际签名 pack/compose/verify/publish、scope 元数据、统一开发/发行工具链；临时测试根不能冒充生产根。
- R28：`/tmp/mintcat-native-gc-final-proof/native-reachable-evidence.json`，native scope 激活/并行更新/局部回退、真实文件输入与产物 hash、GC 不删除仍引用的 Mod/安装计划、APFS error 28 恢复；不是完整资源/安装更新验收。
- 用户三条问题均有新入口实证：RC cleanup `/tmp/mintcat-rc-cleanup-native-canonical/evidence.json`；per-game settings `/tmp/mintcat-native-gc-final-proof/native-reachable-evidence.json`；声明名称的真实 owned process `/tmp/mintcat-game-guard-final2-0b2wodqu/evidence.json`。不以改 argv 名称代替实际可执行文件身份。
- 参考图视觉重建的历史基线：`/tmp/mintcat-original-ui-baseline.Szae8l/runtime.json` 已被 `IGjigV/runtime.json` 的 9 包/8 UI 替代；该轮真实 canonical 注册/Profile/group/批准选择/import/rename/move 见 `/tmp/mintcat-normal-native-ui.pZoGQQ/scenario.json`。此前发布的 `native-live-light/native-surface.png`、`native-saved-dark-table/native-surface.png` 保留业务证据，但其外部桌面/窗口留白已被本轮原生窗口修正覆盖，不作为当前布局验收。
- 历史参考图框架的 `pnpm check`、3 个行为文件 9/9、module-dev shared runtime 与桌面/`custom-protocol`/daemon/CLI 构建通过（`artifact://1451`、`artifact://1454`）；IPC lifecycle 显式 `--features fixtures` 4/4，未开放生产 fixture gate。此前 Chrome 的 1568×882/1484×838 外框测量仅为历史预览；当前窗口无该外部背景/边距，验收尺寸和实际原库内容见下面 2026-10-01 evidence。浏览器预览不替代 signed Library/native 业务。
- 原生 consumer 实跑复现同步 `modules.list` inventory 在 Tokio IPC worker 校验完整签名 bytes 6,491ms，期间另一条 hello 无 5 秒预算内 ack；改为 blocking worker 并保留 `UiCallAdmission` 到完成。`/tmp/mintcat-normal-native-ui.i7A5wc/native-transport-after.json`：真实 9 包 inventory 6,220ms，两个 nested inventory 5,801/11,593ms；其中六条并发 hello 全部 0–1ms，games/profiles/mods/groups 9–13ms，真实 Mod/8列已渲染。观测器之后的长等待导致 session 过期仍正确返回 `STALE_SESSION`，不作为干净视觉图，不弱化 gate。
- 新多页面 Library 的 module-dev caller 已切为一次只挂选中 signed page；真实浏览器从首页面到 actual worker details 再返回，旧页均清除，记录 `/tmp/mintcat-reference-dev-pages.39RyRS/evidence.json`。已停止开发服务并删除 source/dev/key scaffolds；该 proof 仅证明开发页面选择，不冒充游戏或原生业务验收。
- 深/浅 palette-specific CSS variable key 与同步 theme commit 保留组件/draft lifetime；`/tmp/mintcat-ui-theme-portal-evidence.json` 实际 Chromium 三 root + 外置 portal 的 settled 选择器颜色为深 `rgb(232,237,248)`、浅 `rgb(38,41,54)`，编辑 state 保留且无渲染错误。原生隐藏 view 黑字的最终根因不是 token（实际 ant/root vars 已是 `#e8edf8`），而是 WebKit inactive clock：`pZoGQQ/native-inactive-before.json` 墙钟673,003ms、document timeline 0ms、policy0、session失效与 barrier 无应答。公开 `background_throttling(Disabled)` 后真实 policy2、heartbeat/页面/READY恢复；已保存的真实 Dark 偏好正常重开验证可读深色图。LaunchServices/SystemEvents正常激活仍未获得前台可见性，隐藏 CSS animation clock 不推进；不改 visibility/动画/TTL、不把隐藏动态过渡或按钮启用中间色当作前台视觉验收。完整 check/9项行为构建 `artifact://1496`，背景策略 native build 已通过。
- 实际 prepare 的同步签名校验/owner callback 也搬到 blocking worker，`3ZOgBg/native-update-after-offload.json` 的并发 hello 最大1ms；删除对已由 native `StateOwner.drain` 排空的 stateful owner 重复调用 stateless-only supervisor 的七行旧路径，保留一致备份/迁移/确切 native readiness/只读 health/首写 gate；14个更新行为回归通过 `artifact://1513`。
- 真正 native `s9a0u3/native-lifecycle-evidence.json` 在 prepare/Probation/三 session READY 后重复同 barrier 21次，无RPC错误；根因 `ui_barriers` 把终态 participant 仍作为 outstanding 返回。改为按 session 排除 READY/FAILED，不删除事务 barrier、不提前释放其他 participant/Probation gate。两客户端+coordinator重启回归：修复前 `artifact://1554` 失败，修复后14项通过 `artifact://1556`；当前 signed native `EvJc4G/native-lifecycle-evidence.json` 恰好3次 Disposed/3次 Ready、confirmed，无重复投递或 outer/nested business 错误。
- R23：`/tmp/mintcat-native-workbench-proof.EvJc4G/native-lifecycle-evidence.json`，真实签名 ESM 挂载、禁用移除/启用恢复 contribution、新增页面、同步及后续 React render 坏页隔离、SafeMode 均通过。GUI 正常退出0；daemon-owned Agent job `992cb4b9-ccea-4e07-94f2-1e0212786c22` 关闭前后仍 running，释放真实本地 HTTP provider 后 succeeded；未声称该 job 由 UI 发起。
- Library probation 不再把查询也当 writer 暂停：只读 health 通过当前 schema/integrity 后启用独立 READ_ONLY reader，仅明确查询可用；变更仍返回 library-paused，pause/open_writes/数据库消失使 reader 失效。8 个 native owner、14 个 update、4 个 IPC fixture 行为测试通过 `artifact://1628`；当前真实 native 热切换的 probation profiles.list 成功，outer/nested business 错误为0。旧 schema/损坏 SQLite 拒绝，不初始化/迁移或抢 writer lock。
- R29 同页 live 证明：`/tmp/mintcat-native-workbench-proof.TdOOqV/native-dirty-live-evidence.json`。真实输入先 Dirty→Deferred 保留，再经 formal context.saveState/state/dirty 保存并 Disposed；修复仅允许精确新 generation 在写入围栏下挂载、成功才 READY 后，不退出重开即出现 v2 页面、实际 input.value 和 SAVED 草稿。该 participant 恰好一次 Disposed/Ready，最终 confirmed，outer/nested errors为0；实际图为 `native-dirty-restored/native-surface.png`。不以 URL 更新、重启或只看 READY 代替 live 挂载。
- 最终同页修复后统一 `pnpm check`、3 个行为文件9/9、生产 UI build、native custom-protocol host build 通过 `artifact://1639`；随后使用该 host 与 IGjigV 基线完成上述 strict live native 场景。
- 在线模组的 Ant Design 6 Select/共享圆角样式已修正。`/tmp/mintcat-normal-native-ui.4pORUN/native-search-reference-evidence.json` 的实际 signed Light 工具栏/筛选控件为10px玻璃圆角，Dark 选中文本为 rgb(232,237,248)，外层面板12px/1px glass正常。`native-dark-transition-evidence.json` 证明签名 CSS fetch200/hash与当前文件一致、目标 keyframe 正确，但隐藏 Dark transitions 的 currentTime/startTime/progress 为0/null/0；真实前台不可用，不能将首帧白边截图记为 settled Dark 控件通过。可见 Chromium 重放仅为样式诊断，不替代该原生验收。
- 验证结束后已删除父代理3个临时 producer 和原生观察者10个 throwaway scripts；按完整 argv/data-dir 核对并正常停止已识别的 fixture 原生 launcher/daemon/worker，最终无这些 owned root 或 debugger 残留。Detached 进程不伪称 exit0；全部签名包、manifest、隔离数据和截图证据保留，未知 ownership 的进程未触碰。
- Nexus paid callback 真实 worker/loopback HTTP 已验证 exact mod/file/game 身份、4 次请求、错误引用拒绝、expired key 无授权 HTTP；没有 Premium 生产账号，不计真实网页登录。
- 2026-10-01 原库数据/原生窗口证据：`/tmp/mintcat-original-data-ui.ngOCtt/native-real-data-ui-evidence.json`。通过 `sqlite3 -readonly` 一致备份原 `com.mint.cat/mintcat.sqlite`；仅副本凭据值脱敏。独立 canonical 导入 job `a76d2192-d903-477b-bdfc-5ea922078456` succeeded：2 个游戏、2 个安装记录、2 个 Profile、3 个 group、232 个 Mod、84 个 membership、303 个版本、17 个 setting；实际迁移凭据为0。原库、另一原数据库及 WAL 的前后 SHA-256 相同；不覆盖唯一生产副本。
- 本轮 `signed/runtime-revision2.json` 为8包/7 UI、Proxy 未准入的隔离开发签名基线，7个 UI 使用不可变新版本0.1.1。SDK glue 打进 ESM、React/Ant Design 由宿主共享 binding 解析；裸 SDK import 的错误先在 native 复现，修正后按新版本发布，未覆盖 admitted 相同版本/digest 或绕过签名/sequence gate。
- 原生 AppKit 证明真实 Close/Minimize/Zoom 三个可见 `NSButton`、styleMask15、transparent titlebar/hidden title；窗口1500×932、内容1500×900。HTML 窗口按钮为0，body background-image为none、padding0，内容从 `(0,0)` 铺满，logo 为 `(14,16,143,38.28125)`。仅保留系统标题栏，不留 HTML 假按钮空白。
- 实际 native UI 从原库显示 DRG Default 68 条/68已启用、Rogue Core 16 条。版本、类别、多人要求、认证均来自真实 Library 数据；未标明/sentinel/歧义 current 不伪造版本，未证明游戏版本显示 `—`，无已应用版本/安装 receipt 的假声明。233条导入警告中，232条旧 cache hint 不视为已验证 artifact，另1条为未迁移凭据。
- 原生 UI 与 canonical API 联合证明：Steam Helper 禁用后查询为false、恢复后true/68条；`Damage` 搜索恰好 Damage Meter 与 Damage Indicator 2；两游戏选择器切换；克隆68条到本轮验证 Profile、重命名、只删除该验证副本，最终原2个 Profile 保留、DRG Default 再次显示68条。安装目录设置页显示原 Windows 路径记录，不在 macOS 上伪称验证了该游戏输入。
- 本地导入/下载管理真实加载 Profile 并提供文件/目录选择。两个 durable selection 经实际 `取消选择` 返回 failed/`INTERACTION_REJECTED`，未批准或导入未选择文件，终态历史继续可查。设置页云备份明确返回缺少 operator-configured HTTPS endpoint 的 `CLOUD_PREREQUISITE`，不拿 mock history 或本地归档冒充生产云服务。
- 无 Proxy 的离线备份 job `4ab24bbb-f0bd-44cd-83c4-bfc4520d0423` succeeded，format2、ownerSchemas只含 Library5/Agent1、232个Mod/84个membership、凭据策略references-only。隔离 Proxy 遗留文件的反例 job `85745235-2448-46c3-8198-d824c09a3a3f` 正确 failed/`backup.owner_unavailable` 且未输出归档；临时文件已移除。未取消批准、maintenance、owner schema或epoch围栏，未改证书/hosts/系统服务。
- 本轮 `pnpm check`、Library22项、桌面3文件11项、daemon lib5项、IPC显式fixtures4/4与native custom-protocol desktop/daemon/CLI/Library worker构建通过。实际 Light/Dark内容图 `native-original-data-light-restored.png`、`native-original-data-dark-restored.png` 已读取并发布；选择器文本分别rgb(38,41,54)/rgb(232,237,248)。图为公开WKWebView内容快照，不包含系统标题栏；隐藏 view 动态动画仍不冒充前台 settled transition，不改visibility/动画/clock/TTL。可达 UI/副本数据通过不等同真实游戏资源/Windows/生产交接或完整R35矩阵完成。
- 本轮 proof 结束后通过公开 AppKit 请求正常退出，debugger 无剩余 session，实际进程表中该 owned root 的进程为0；按完整 argv/data-dir 核对后停止两个验证 daemon 与 preview dev/Chrome 服务。删除4个 producer、观察者源码/4个dylib、15个临时 source copy、私有测试签名 key、临时 entitlements 和 owned Chrome profile；保留签名包/公开元数据、原库副本、离线备份与截图/evidence。未观测 GUI exit code，不伪称exit0，未停止其他用户服务。
- 2026-10-01 列表拖拽修复证据：`/tmp/mintcat-drag-order-proof.zJy2VQ/native-drag-order-evidence.json`。旧签名 UI 在原库数据的验证 Profile 中实际复现：来源视图 Steam→本地只改隐藏 groupId、不改 source；普通分组拖动误变成嵌套。该验证 Profile 已通过 canonical API 删除。固定 UI 在新隔离签名 Library0.1.2 中从同一脱敏副本导入；不覆盖原包版本/digest，也不触碰原数据库。
- 新原生 UI 验证模组向下/向上插入、普通分组同级重排与移出嵌套、Shift 显式嵌套、子树循环拒绝、配置分组跨组移动。跨来源及同来源/不同隐藏配置分组的非法拖动均不改 contentRevision；循环拒绝也无写入。已按完整 argv 确认并重启仅本轮验证 daemon，再重开真实 native 窗口，Damage/Steam 顺序与两组 root 顺序保留。最后恢复68条 membership、全部启用状态及每组相对顺序；非物理鼠标验证，使用公开 WKWebView 中的标准 DragEvent/DataTransfer 与真实 Application API。
- 上/下插入与完整 siblings/稳定 ID 并列顺序、来源/隐藏分组边界、普通重排/显式嵌套、自身/子树循环的行为回归：修复前2项失败；修复后 `pnpm check`、桌面3文件16项、生产 UI build 通过。实际内容快照 `native-drag-insertion-indicator.png`、`native-drag-order-after-reopen.png` 已读取，插入线与重开后的顺序有原生视觉证据；不修改 visibility/动画/clock/TTL，不增加 R35 整体验收声明。
- 拖拽 proof 已通过公开 AppKit 请求退出，debugger 无 session，两个 proof root 的进程表记录为0；仅停止完整 argv/data-dir 确认归属的两个 daemon。已删除本轮 producer、观察者源码/dylib、临时 UI/source copy 和私有测试签名 key，保留签名包、公开元数据、隔离数据及截图/evidence。未观测 GUI exit code，不宣称 exit0，未触碰其他用户进程。

### E.2 每项未完成任务的实际位置

| 任务 | 当前状态 | 已有真实产物/证据 | 仍未完成的验收前提 |
| --- | --- | --- | --- |
| R22 | 外部输入阻塞 | canonical install 路由、固定 worker 闭包、native 两游戏 pack/local import；`install.plan` 结构化拒绝缺失资源 | 缺少授权 `resource.drg.patch` / `resource.rogue-core.patch` 签名包与真实游戏输入，不能绕过 requirement 完成两游戏 apply/uninstall/receipt |
| R25 | 已取消继续接入，非系统验收通过 | 保留独立实验 service、原版本 lease/protocol stop 与历史 CA-approved TLS forwarding `/tmp/mintcat-proxy-application.aK8GEy/evidence.json` | 当前路线无此阻塞；未来恢复实验仍须单独处理授权和许可，不修改系统 trust/hosts |
| R30 | pack/局部回退通过，资源/receipt 阻塞 | DRG engine v2 / RC v1，RC 更新后 DRG 局部回退、实际 digest/pack hash 与 GC 重新打包 | 独立资源更新、旧安装任务并行及真实 receipt 仍缺 R22 signed resources/game inputs |
| R32 | CLI 已联通，测试安装阻塞 | 实际无 GUI 查询/编辑/local import/pack、durable approve/job/status、受控 Nexus resume、完整单 envelope JSON；已移除旧 bridge aliases | 两游戏完整测试安装被 R22 缺失签名资源阻止；生产 provider 凭据不计受控 HTTP 实证 |
| R33 | macOS standard/CLI 通过，目标矩阵阻塞 | `/tmp/mintcat-canonical-release-proof-r7ZaUP` 验签 staging、真实 readonly DMG 和解压 CLI 在无 dev env 下启动离线 signed baseline | 缺干净 Windows standard/portable/fixed-WebView2 目标、签名授权和完整游戏资源；Proxy 实验不再是前置 |
| R34 | 真实原库游戏副本已验证，生产交接未完成 | 一致只读副本经 canonical/native 导入；2 游戏、2 配置、232 Mod、84 配置关联，凭据迁移0；原始数据库/WAL 哈希保持不变 | 真实凭据迁移、生产覆盖与完整发行切换未授权，仍不能触碰用户唯一旧数据库 |
| R35 | 可达场景已跑，完整矩阵阻塞 | 上述 native API/UI/CLI/worker/更新/GC/崩溃恢复证据及原库副本验证 | R22/R30/R32–R34 的输入/权限/目标平台前提未消除；R25 已取消，不宣称最终整体验收 |

### E.3 当前优先工作与风险

1. 当前 UI 使用原库真实游戏/模组副本，继续通过统一 Application API 验证可达交互；不以合成数据、只读列表或浏览器预览冒充原生业务操作。
2. 保持原生窗口标题栏/三色按钮、无外部桌面背景、Logo 紧靠左上内容角；不恢复 HTML close/minimize/maximize 按钮或 UI 窗口操作权限。
3. R22/R30/R32 只等待授权签名 patch/resource 和真实游戏输入；R33 等待授权干净 Windows 环境；R34 等待凭据及生产切换授权。R25 不再参与依赖。
4. 跨 scope 回退不覆盖其他游戏；新写入后不恢复旧 DB。缺失真实 migration provider 时拒绝升级，不以旁路或假健康补验收。

### E.4 外部验收前提

- 没有受认证的生产 trust-root、私钥或线上签名/上传权限；临时测试根只证明隔离演练。
- 没有授权真实 DRG/RC base PAK/版本矩阵与相应 signed patch/resource 输入。
- Windows standard/portable/fixed WebView2 与真实文件锁需授权目标设备；当前 macOS 证据不代替该矩阵。
- Proxy 继续接入已取消。原独立源码未声明 LICENSE 的事实保留，但系统 trust/hosts、高权限和 Proxy 再分发不再阻塞当前路线；取消不授予许可。
- 原库授权的一致只读游戏数据副本已验证；真实凭据迁移、生产写入/切换和生产 provider/LLM/云账户输入仍未授权。
- 不兼容 schema 升级必须具备真实签名 owner/backup/migration/restore provider，缺失时拒绝并保留数据。

根 `TODO` 保留三条用户问题及实际证据边界；以上前提不授权旁路安全边界、伪造 receipt 或宣称生产/Windows 发布成功。

