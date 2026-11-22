# MintCat 旧版本功能点清单与当前分支差异测试报告

> 生成日期：2026-11-19  
> 对比范围：
> - **主分支（`origin/main`）**：commit `f9d4d31`（`v0.5.0-dev`，单游戏 DRG、JSON 文件存储、直连 Tauri 后端）
> - **旧版开发基线（`dev`）**：commit `da5b92f`（`v0.6.0-beta.4`，重构前最新功能全集：SQLite、多游戏 DRG/RC、多源搜索、云备份、Agent、Proxy 加速器、热更新）
> - **当前分支（`feat/modular-runtime-glass-ui`）**：commit `f3b7f1d`（独立 Rust Daemon `mintcatd` + 类型化 CLI `mintcat` + 毛玻璃 Workbench 桌面端 + 独立签名游戏插件）

---

## 1. 总体结论与状态统计

当前分支完成了从「前端直连 SQLite + 进程内 DLL 加载」到「独立 Daemon 单写者 + 统一 68 条 JSON-RPC 契约（`packages/contracts/commands.json` v2.0.0）+ 持久 Job/Action 审批 + 原生毛玻璃 UI + 独立签名游戏插件」的彻底架构重写。

对比两个旧版基线，按 12 个功能域逐项拆解出 **79 个细粒度旧版功能点**（涵盖 `origin/main` 全部功能以及 `dev` 新增功能），当前分支覆盖情况如下：

| 状态分类 | 数量 | 说明 |
| --- | ---: | --- |
| **已实现** | 22 | 核心能力完整实现，且在事务一致性、并发隔离或安全性上显著增强 |
| **实现但有差异** | 36 | 能力存在，但交互方式、参数模型、配置方式或约束语义与旧版不同 |
| **仅后端/契约已实现** | 4 | 后端 crate / Daemon / CLI 契约已验证通过，但缺少默认 UI 入口或受控宿主桥接 |
| **未实现** | 16 | 旧版具备但当前分支缺失或已移除的功能 |
| **已取消（保留实验代码）** | 1 | 按产品决策停止默认接入，仅保留可选编译实验模块（Proxy / R25） |
| **当前分支新增（超出旧版）** | 14 | 仅当前分支新增的核心能力（单列于第 4 节） |

### 最关键的功能缺口与差异速览

1. **未实现 / 缺失的高感知功能**：
   - **一键启动游戏（`steam://run/...`）与「启动前自动安装」**：新版主按钮仅为「安装模组」，彻底无启动游戏 API/按钮。
   - **游戏内/界面多语言（i18n 中英文切换）**：新版前端界面硬编码简体中文，移除了 `i18next` 与语言切换设置。
   - **账号与凭据管理 UI（`UserSettingDialog`）**：新版无 mod.io / ModCat / Nexus Mods / MintCat OAuth 令牌录入界面、无用户头像/ID 展示、无 VIP 支持者状态检测与 `mintcat://` / `nxm://` Deep Link 回调监听。
   - **粘贴链接批量添加 Mod & 剪贴板自动监控 & mod.io 订阅同步**：新版搜索页仅支持关键词搜索，无法直接粘贴 URL（`OnlineTab`），无剪贴板监听自动弹窗，也无 `mod.io Subscribed` 订阅列表导入。
   - **Mod 列表检查更新 / 批量更新 / 元数据刷新 / 过期红点告警**：新版无主动轮询在线更新（`CheckModUpdateTask`）入口，无列表内红点/时钟过期提示，无「批量更新」操作。
   - **Mod 独立文件冲突检测 UI（`ModConflictCheckTask`）**：虽然 `modules/engine-unreal-pak` 在打包/安装阶段会产出 `conflicts` 报告，但 Mod 管理页没有独立的「检测冲突」按钮和冲突详情展示。
   - **列表辅助功能缺失**：复制单个 Mod 链接、复制整个列表 URL、导出单个 Mod 缓存文件（`.zip`/`.pak`）、标签/来源/审批状态多选组合筛选（旧 `SearchBox`）、按时间/认证状态排序、搜索结果一键翻译（Google 翻译代理）、新手引导（`OnboardingTour`）、应用整包自动更新弹窗（`UpdateDialog`）、开发者工具按钮、单实例防多开插件、内置崩溃上报（Sentry/minidump）。
   - **C# Mod（`UE4SSL.CSharp.dll` / `csmods`）与 .NET Runtime 安装**：`origin/main` 最后一个 commit 引入了 C# Mod 与 `csmods`，`dev` 引入了 .NET Runtime 支持；当前 `modules/loader-ue4ss` 仅实现了 Signed Resource、JS（`js/main.js`）、Native DLL（`main.dll`）与音频 PAK（`*_P.pak`），未显式覆盖 `csmods` C# Mod 和 .NET 运行时安装。
   - **游戏补丁二进制资产缺失导致无法端到端真实安装**：仓库内已移除 `DRG.zip` / `RC.zip` / `UE4SSL.zip` / `x3daudio1_7.dll` / `oo2core_9_win64.dll`，当前 `mintcat-module check/pack` 在未提供签名 `resources/drg-patch.zip` 与 `resources/rogue-core-patch.zip` 时会主动拒绝打包。
2. **实测发现的文档/实现偏差与重要约束**：
   - **CLI `mintcat status` 会自动拉起 Daemon**：`README.md` 声明“`status` does not initialize storage or start a daemon”，但 `apps/cli/src/main.rs` 中所有命令（包括 `status`）统一走 `Connection::connect -> connect_or_start`，实测对不存在目录执行 `mintcat --data-dir /tmp/mc-status-check status` 会自动创建目录并拉起 `mintcatd`；仅 `mintcat environment` 和桌面端的 `daemon_status` 不自动启动。
   - **默认无 `runtime.json` 时在线搜索源为空且游戏设置不可更新**：未配置 `MINTCAT_RUNTIME_CONFIG` 或打包 `runtime.json` 时，`sources.list` 返回 `[]`，`modules.list` 返回 `[]`；且 `settings.update` 对 `scope.kind = "game"` 强依赖已激活的游戏适配器插件做 schema 校验（无插件时只允许 `settings.get` 读取，修改返回 `MODULE_UNAVAILABLE`）。
   - **备份导出拒绝覆盖时任务进入 `recovery-required`**：实测对已存在路径执行 `backup.export` 会拒绝覆盖且保持原文件完整，任务终态记为 `recovery-required`（错误码 `CONFLICT`），需要 `backup.recover` 检查维护态。
   - **旧版安装自动补下内置资产与发布渠道切换**：旧版 `dev` 在设置页提供 `alpha`/`beta`/`stable` 发布渠道切换，安装前自动从 OSS 下载 `DRG.zip` / `RC.zip` / `UE4SSL.zip`；新版取消前端渠道切换与自动拉取未签名 ZIP，改为通过 `updates.channel` / `updates.stage` 走签名 TUF 仓库管理游戏包。

---

## 2. 旧版本功能点逐项细节清单与对比矩阵

> **图例说明**：
> - **来源基线**：`main` = `origin/main` 已有；`dev` = 仅 `dev` 新增/重构；`main+dev` = 两者均有（`dev` 可能有演进）。
> - **状态**：`已实现` / `实现但有差异` / `仅后端/契约已实现` / `未实现` / `已取消（保留实验代码）`。

### 2.1 应用壳层、窗口、导航与全局交互

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| A01 | 窗口框架与标题栏 | `main+dev` | Tauri 桌面窗口，顶部 `TitleBar.tsx` 含彩色渐变 `MINTCAT` 标题、版本号、启动按钮、云同步、通知铃铛、皮肤弹窗、帮助/引导、Agent、头像；底部 `StatusBar.tsx` 显示进度条与日志 | `apps/desktop/src/chrome.ts` + `window-surface.ts`：macOS Overlay 标题栏+系统红黄绿按钮+原生 `NSVisualEffectView` 毛玻璃；Windows 11 22H2+ DWM Acrylic；左侧 172px 标签侧栏；顶部动态工具栏 Portal；底部状态栏含后台连接态与「诊断与恢复」 | **实现但有差异** | 代码核对 + `workbench-behavior.test.ts` |
| A02 | 左侧导航菜单 | `main+dev` | 50px 图标窄栏：`Home`、`mod.io`（`dev` 改为多源 `SearchPage`）、`Settings`；`main` 曾有注释掉的 `ChatPage`（Easemob iframe），`dev` 曾有注释掉的 `Accelerator` | 172px 带文字侧栏：固定「模组管理」「在线模组」「本地导入」「下载管理」「设置」；顶部快捷入口提供「Agent 助手」「安装与打包」及更多菜单 | **实现但有差异** | `apps/desktop/src/workbench.ts` + `workbench-behavior.test.ts` |
| A03 | 主题切换与持久化 | `main+dev` | `main` 支持 Light / Dark / Pink 3 套外置 CSS；`dev` 扩充为 Light(紫) / Blue / Dark / Pink 4 套，存数据库与 `localStorage` | 支持 Light（浅色玻璃）/ Dark（深色夜幕）/ Pink（樱粉微光）/ Blue（晴空冷萃）4 套主题，通过 CSS 变量 + Antd ConfigProvider + 原生材质联动即时切换，支持系统「减弱透明度」无障碍感知；存储仅用 `localStorage`（不写入 Library DB） | **已实现** | `settings-overview.ts` + `packages/sdk-ui/src/react-view.ts` + `react-view.test.ts` |
| A04 | 多语言切换（中/英文 i18n） | `main+dev` | `i18next` + `src/locales/zh-CN.json`、`en.json`；设置页可切换中英文并刷新生效；`dev` 增加后端安装进度 i18n 键 | 前端全部文案硬编码简体中文，无 `i18next`，无英文翻译文件，设置页无语言选项；仅后端错误携带 `messageKey`（如 `source.operation.failed`） | **未实现** | `apps/desktop` grep `i18n` 无结果 |
| A05 | 禁用 `Ctrl+F` 浏览器查找 | `main+dev` | `App.tsx` 监听 `Ctrl+F` 并 `preventDefault()` 阻止 WebView 默认查找框 | 未拦截 `Ctrl+F`；新增 `Cmd/Ctrl+K` 聚焦模组列表搜索框 | **实现但有差异** | `modules/library/ui/library-view.tsx` |
| A06 | 单实例防多开（Single Instance） | `dev` | `tauri-plugin-single-instance` 阻止第二进程并聚焦主窗口，同时转发 `mintcat://` / `nxm://` 参数 | 桌面端未集成 `tauri-plugin-single-instance`；改为 **Daemon 按 data-dir 独占文件锁**（`runtime/ipc`），多客户端共享同一 Daemon 实例，但未阻止启动多个桌面窗口 | **实现但有差异** | `apps/desktop/src-tauri/Cargo.toml` + `mintcat-runtime-ipc` 单测 |
| A07 | Deep Link 协议处理（`mintcat://` & `nxm://`） | `dev` | `tauri-plugin-deep-link` + `useDeepLinkHandler.ts` + `useOAuthCallback.ts` 接收系统拉起的 OAuth 回调与 Nexus NXM 链接 | 桌面宿主未注册 `tauri-plugin-deep-link`；`nxm://` 回调逻辑在后端 `runtime/broker` + `modules/source-nexusmods` 中实现为 durable `browser-download` Action，但桌面端无 Deep Link 自动捕获接入 | **仅后端/契约已实现** | `apps/desktop/src-tauri/Cargo.toml` + `mintcat-source-nexusmods` 6 项测试 |
| A08 | 新手指引（`OnboardingTour`）与帮助文档 | `dev` | 首次启动触发 12 步 Antd `Tour` 引导（欢迎→配置导入→游戏选择→用户设置→首页→配置→安装→添加→启动→在线→设置→结束）；标题栏可重看引导或外链打开 `https://www.mintcat.work`（`main` 为 `https://mintcat.v1st.net`） | 完全移除新手指引与帮助文档外链按钮；无插件/空配置时以 Empty Hero 卡片展示引导文案 | **未实现** | `apps/desktop/src/chrome.ts`、`library-view.tsx` |
| A09 | 开发者工具开关（Open DevTools） | `main+dev` | 设置页「打开工具」按钮调用 `open_devtools` 开关 WebView DevTools | 桌面宿主未暴露 `open_devtools` 命令，设置页无入口（安全边界收紧） | **未实现** | `apps/desktop/src-tauri/src/lib.rs` |
| A10 | 崩溃转储与上报（Sentry / minidump） | `main+dev` | `dev` 集成 `tauri-plugin-sentry` + `minidump`；`main` commit `2debfad` 加入 crash dump | 已移除 Sentry 与 minidump，改为 Daemon 日志文件 `runtime/daemon.log` + 独立 Rescue 面板查看最近 64 KiB 日志 | **未实现** | `apps/desktop/src-tauri/Cargo.toml` + `rescue_log` |
| A11 | 状态栏实时进度条与日志 | `main+dev` | 底部 `StatusBar.tsx` 订阅 `status-bar-log` / `status-bar-percent`（`dev` 接入 `tauri-plugin-task-queue` 展示分级彩色日志与任务进度） | 底部状态栏仅展示后台连接态与单条最新通知；详细进度、任务阶段与事件流迁移至「下载管理」「安装与打包」及各页持久任务面板（`jobs.watch`） | **实现但有差异** | `apps/desktop/src/chrome.ts`、`library-view.tsx` |
| A12 | 应用内旧版聊天页（`ChatPage`） | `main` | `src/pages/ChatPage.tsx` 嵌套环信网页 iframe（菜单中已注释） | 已删除（`dev` 已在 `83a2593` 移除） | **未实现** | 历史已废弃 |

---

### 2.2 多游戏管理与游戏目录识别

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| G01 | Deep Rock Galactic（DRG）支持 | `main+dev` | Steam (`FSD-WindowsNoEditor.pak`, AppID `548430`) 与 Xbox (`FSD-WinGDK.pak`)；`main` 硬编码单游戏，`dev` 存入 `games` 表 | 独立签名插件 `modules/game-drg`（`game:drg`），声明 `steam` / `xbox` 双变体、精确主 PAK 校验、进程名（`FSD-Win64-Shipping.exe` / `FSD-WinGDK-Shipping.exe` / `FSD.exe`）；默认 builtin 构建不内置，需加载签名游戏包 | **实现但有差异** | `modules/game-drg` 4 项测试 + `smoke-games.mjs` 实跑通过 |
| G02 | Rogue Core（RC）支持 | `dev` | `games` 表中 `name='rc'`，校验 `RogueCore-Windows.pak`；存在若干硬编码回退 DRG 的遗留问题 | 独立签名插件 `modules/game-rogue-core`（`game:rogue-core`），区分 `steam-release` (`2605790`) 与 `steam-playtest` (`2860770`)，显式进程名（`RogueCore-Win64-Shipping.exe` / `RogueCore.exe`），校验 V11 + `AssetRegistry.bin`；旧库导入时自动将 `rc` 规范化为 `game:rogue-core` | **已实现** | `modules/game-rogue-core` 7 项测试 + CLI 旧库导入实测 |
| G03 | 游戏切换界面 | `dev` | 标题栏 `SelectGameDialog` 弹窗选择游戏、高亮当前激活游戏、切换发出 `active-game-change` | 模组管理与在线模组顶部工具栏内直接提供游戏下拉框 `Select`，无独立弹窗；且支持在未安装游戏插件时继续只读/编辑已导入旧库中的游戏元数据 | **已实现** | `library-view.tsx` + `workbench-behavior.test.ts` |
| G04 | 自动查找游戏安装路径（Steam） | `main+dev` | `find_game_pak` 调 `steamlocate` 找 `548430`（`dev` 增加 RC）主 PAK 并直接写回路径 | 契约 `games.discover`（只读发现，不产生写副作用）返回候选安装目录列表，需用户显式选定再调用 `games.register` 经原生 worker 验证主 PAK 指纹后入库；UI 位于「游戏设置与安装目录」弹窗 | **已实现** | `library.ts` + `smoke-games.mjs` discovery 实测通过 |
| G05 | 手动浏览选择游戏路径 | `main+dev` | 调 Tauri 原生文件选择弹窗（`@tauri-apps/plugin-dialog`）过滤 `*.pak` | 桌面端已移除 `@tauri-apps/plugin-dialog` 权限，UI 中改为**手动输入游戏安装目录绝对路径文本框**，再点击「发现」或「验证并注册」 | **实现但有差异** | `library-view.tsx`（纯 `Antd.Input`）+ `capabilities/default.json` |
| G06 | 一键启动游戏（Launch Game） | `main+dev` | 标题栏主按钮：若当前配置有 Mod 先自动安装，再执行 `steam://run/<appId>` 启动游戏（`dev` 支持多游戏 AppID） | **未实现**：新版无 `launch_game` 命令或 Steam 启动逻辑，主按钮仅跳转「安装与打包」页面执行计划/批准/安装（`ChangeLog` 明确说明不伪造启动游戏 API） | **未实现** | `commands.json` 68 条契约无 launch 命令 + `chrome.ts` |
| G07 | 每游戏独立打包/运行设置 | `dev`（有缺陷） | `dev` 中 UE4SS 开关和 `compressModPak` 为全局设置，两游戏共享（根 TODO 遗留缺陷） | **增强已实现**：`settings.get` / `settings.update` 按 `scope: { kind: 'game', gameId }` 强隔离，DRG 与 RC 各自拥有 schema 驱动的 `compression`（`none`/`zlib`）和 `ue4ssEnabled` 设置与独立修订号；修改需加载对应游戏插件以校验 schema（实测传入非法值 `oodle` 返回 `INVALID_ARGUMENT`，无插件时修改返回 `MODULE_UNAVAILABLE`） | **已实现** | `workbench-behavior.test.ts` + 真实配置运行时 CLI 实测（DRG 设置更新 + 非法枚举拒绝） |

---

### 2.3 Profile（配置）与分组树管理

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| P01 | Profile 列表与切换 | `main+dev` | 下拉框切换当前 Profile；`dev` 查询时若无激活 Profile 会隐式创建/激活默认配置 | `profiles.list` 纯只读无副作用；工具栏与「管理配置」弹窗均可切换显式 `profileId`；所有变更请求携带捕获的 `profileId` + `revision`，切换 Profile 不会污染在途请求 | **已实现** | `mintcat-library-service` 15 项测试 + CLI 实测 |
| P02 | 新建 / 重命名 / 复制 / 删除 Profile | `main+dev` | `ProfileEditDialog`：新建（限制非空、≤20 字符、防重名）、行内重命名、复制为 `xxx_copy`、删除（至少保留 1 个） | 完整支持 `profiles.create` / `profiles.update`（含名称与说明编辑）/ `profiles.clone`（修订绑定复制整棵树与启停状态）/ `profiles.delete`（删除当前激活 Profile 不会隐式激活另一个）；全部需要幂等键与乐观锁修订号 | **已实现** | CLI 实测 `clone` & `delete` + `workbench-behavior.test.ts` |
| P03 | 默认分组（`mod.io` / `local`）与自定义嵌套分组 | `main+dev` | `main` 固定不可删的 `MODIO` / `LOCAL` 默认根组 + 任意嵌套子分组；`dev` 改为 SQLite `profile_folders`，允许删自定义组但至少保留 1 组 | 新版双视图模式：默认「按来源分组」（按 `mod.io`/`Nexus Mods`/`ModCat`/`本地导入` 动态折叠分组，只读展示）+ 可切换「配置分组」（完整对应 `groups.list/create/update/move/delete`，支持多级嵌套子分组、展开态持久化、删除叶子组时保留至少 1 组） | **实现但有差异** | `library-view.tsx` + `mintcat-library-service` 测试 |
| P04 | 分组增删改与排序移动 | `main+dev` | 右键菜单：添加新分组、添加子分组、重命名分组、删除分组；`dev` 支持文件夹拖拽排序和移动 | 「配置分组」视图右键/更多菜单完整支持：添加子分组、重命名分组、移动到指定父分组/根分组、上移、下移、删除分组；支持拖拽同级排序和 `Shift+拖拽` 嵌套移入（带防循环嵌套保护） | **已实现** | `library-view.tsx` + `library-model.test.ts` |
| P05 | Mod 列表树拖拽排序与跨组移动 | `main+dev` | `DragAndDropTree.ts` + Antd `Tree` 拖拽，写回时先删整棵树再重建；拖拽时动态关闭虚拟滚动防跳动 | `mods.move` 原子操作 + `library-model.ts` 上/下半区插入线计算；「配置分组」视图支持跨组移动与组内排序；「按来源分组」视图仅允许同来源且同隐藏配置分组内排序（防意外篡改隐藏分组）；额外支持快捷键 `Alt+↑/↓` 移动 | **实现但有差异** | `library-model.ts` + `library-model.test.ts` |
| P06 | 文件夹展开状态记忆 | `dev` | `HomePage` 记忆并按名称重建 `expandedKeys` | `groups.update` 持久化每个分组的 `expanded` 字段到 Library SQLite，重载后保留 | **已实现** | `library.ts` `expandGroup` |

---

### 2.4 Mod 列表查看、筛选、排序与批量操作

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| M01 | Mod 列表展示与序号/元数据列 | `main+dev` | TreeView 节点展示：开关、版本下拉、状态图标、进度圆环、可点击打开主页的标题、`V`/`A`/`S` 认证标签、游戏版本不匹配红标、`RequiredByAll` 标、普通 Tags；`dev` 增加行序号与冲突警告标 | 紧凑 7 列表格：`#` 序号、名称（含 pending 点与操作菜单）、版本下拉、类别标签（最多 2 个 + `+N`）、多人联机（全员必装/推荐全员/客户端可选）、认证（已验证/已批准/沙盒）、启用开关；**差异**：无游戏版本不匹配标、名称不再点击外链打开主页、无单行冲突警告图标 | **实现但有差异** | `library-view.tsx` + `library-model.ts` |
| M02 | 单个 Mod 启用/禁用与未保存更改提示 | `main+dev` | 点击开关更新启用态；`dev` 通过 `computeInstallManifestHash` 对比上次安装哈希，有差异时「保存更改」按钮呼吸动画高亮 | `mods.setEnabled` 事务更新并推进 `contentRevision`；无「未保存更改」哈希比对高亮提示（安装页以当前修订与收据为准） | **实现但有差异** | `library.ts` + CLI `mods set-enabled` 实测 |
| M03 | 单个 Mod 重命名（跨 Profile 共享） | `main+dev` | 右键「重命名」改 `displayName`，跨 Profile 共享；`dev` 额外保留 `originalName` | `mods.rename` 显式携带 `scope: 'shared'` 与 `expectedEntityRevision`；即使下载锁住 Profile 内容编辑时，共享名称重命名依然可用 | **已实现** | CLI `mods rename` 实测 + `library-view.tsx` |
| M04 | 单个 Mod 置顶 / 上移 / 下移 / 移动到组 / 删除 | `main+dev` | `main` 含删除；`dev` 补充「置顶 (`pin_to_top`)」；删除仅移除当前 Profile 关联 | 右键/行菜单完整支持：置顶、上移、下移、移动到指定组/未分组、删除（仅移除 Profile 关联，保留共享元数据与内容） | **已实现** | `library-view.tsx` + `mintcat-library-service` 测试 |
| M05 | 列表搜索与标签/来源/审批组合筛选 | `main+dev` | `SearchBox.tsx` 支持多选组合过滤：关键词 + `Verified`/`Approved`/`Sandbox`/`RequiredByAll`/`Optional`/`Audio`/`Framework`/`Tools`/`QoL`/`Visual`；`dev` 增加按来源（Local/Modio/ModCat/Nexus）和分组筛选 | 顶部提供防抖关键词搜索（后端 `mods.list` `query` 参数）+ 底部快捷胶囊筛选「全部 / 已启用 / 待处理」；**缺失**按标签、审批状态、来源或分组的多选组合筛选 | **实现但有差异** | `library-view.tsx` |
| M06 | Mod 排序方式 | `main+dev` | `main` 支持：名称升序、名称降序、按时间排序；`dev` 扩充为 6 种：名称 A→Z/Z→A、时间新→旧/旧→新、已验证→沙盒 / 沙盒→已验证 | 工具栏仅支持 **名称 A→Z / 名称 Z→A** 两种（前端逐项调用 `mods.move` 重排）；**缺失**按时间排序和按认证状态（Verified/Sandbox）排序 | **实现但有差异** | `library-view.tsx` + `library.ts` `sortMods` |
| M07 | 批量多选、全选/反选与批量操作 | `main+dev` | 底部多选框开启勾选模式；支持全选/反选、批量删除、批量启用、批量禁用、批量更新（并发下载）、批量复制选中 Mod 链接 | 底部「批量选择」支持全选/取消全选、批量启用（`mods.setEnabledBatch`）、批量禁用、批量删除（循环调用 `mods.remove`）；**缺失**批量更新与批量复制链接；额外支持 `Ctrl/Cmd+点击`、键盘 `Space`/`Enter`/`Delete` 操作 | **实现但有差异** | `library-view.tsx` |
| M08 | 复制单个 Mod 链接 / 复制整个列表 URL | `main+dev` | 右键「复制链接」（在线复制 URL，本地复制路径）；工具栏「复制列表」导出当前筛选下的 mod.io 链接到剪贴板 | **未实现**：新版无复制链接或导出列表 URL 到剪贴板功能（桌面端亦未集成剪贴板插件） | **未实现** | `library-view.tsx` + `apps/desktop/src-tauri/Cargo.toml` |
| M09 | 导出单个 Mod 缓存文件（`.zip`/`.pak`） | `main+dev` | 右键「导出」调保存对话框复制本地缓存文件到用户指定路径 | **未实现**：新版内容由内容寻址 Artifact Store 托管，UI 与契约均无单个 Mod 文件导出入口 | **未实现** | `library-view.tsx`、`commands.json` |
| M10 | 清理缺失的本地 Mod | `dev` | 工具栏「清理缺失的本地 Mod」扫描并移除文件已不存在的本地条目（原 TODO 提到 RC 存在异常） | **已实现并修复范围 bug**：`mods.cleanupMissing` 强制绑定显式 `gameId` + `profileId` + `expectedContentRevision`，仅移除目标 Profile 下 `local_missing = 1` 的关联，保留共享元数据；工具栏与本地导入页均有入口 | **已实现** | `modules/library/service/src/library_api.rs` + `mintcat-library-service` 测试 |
| M11 | 已启用/总数统计标签（`CountLabel`） | `main+dev` | 底部显示「已启用数 / 总数」 | 底部显示「`X 已启用 · Y 个模组`（筛选时附 `· 显示 Z`）」 | **已实现** | `library-view.tsx` |

---

### 2.5 在线搜索与多来源接入（mod.io / ModCat / Nexus Mods / 本地）

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| S01 | mod.io 搜索、详情、版本与依赖解析 | `main+dev` | `ModioApi` + `ModioSearchProvider`：固定 DRG GameID `2475`，支持关键词、5 种排序（最新/热门/高评分/A-Z/Z-A）、无限滚动、依赖自动拉取添加 | `modules/source-modio` 通过 `NetworkBroker` 实现游标分页、排序映射、详情/标签/审核、版本/依赖列表与精确 `file:<id>` 下载解析；**差异**：1) `sources.search` UI 未暴露排序下拉框（`SourceApplication::search` 写死 `sort_field: None`）；2) `mods.add` 未自动递归添加依赖 Mod；3) 默认无配置时不启用 | **实现但有差异** | `modules/source-modio` 7 项测试 + `modules/source/service/src/lib.rs` |
| S02 | ModCat 搜索、详情、版本与下载 | `dev` | `ModcatApi` + `ModcatSearchProvider`：支持 DRG (`drg`) 与 RC (`drgrc`) 切换，展示列表/版本/依赖 | `modules/source-modcat` 完整封装 `ModId`/`VersionId`/`FilesId`，按游戏描述严格隔离 `drg` 与 `drgrc`，空 `PicUrl` 规范化防整页报错；默认需在 `runtime.json` 中配置 | **已实现** | `modules/source-modcat` 5 项测试 |
| S03 | Nexus Mods 搜索与非会员 WebView/NXM 下载 | `dev` | `NexusModsApi`（GraphQL 搜索 + REST 详情/文件）+ `downloadWebview.ts` + FFI `nexus_webview` 自动打开内置浏览器捕获 `nxm://` | `modules/source-nexusmods` 完整实现 GraphQL 游标搜索（保留部分失败告警）、REST 详情/精确 fileId 解析、403 时产出 durable `browser-download` Action 校验导航域名与 `nxm://` 回调；**差异**：桌面端尚未实现自动弹出受控 WebView 窗口来响应 `browser-download` Action，需外部/人工提交回调 | **实现但有差异** | `modules/source-nexusmods` 6 项测试 + `apps/desktop` 代码核对 |
| S04 | 搜索源自动按当前游戏过滤与切换 | `dev` | `SearchPage` 按 DRG/RC 动态筛选可用源（如 RC 排除 mod.io），支持下拉切换 | `modules/ui-search/ui/index.ts` 取「已配置来源 (`sources.list`) ∩ 当前签名游戏描述 (`sourceBindings`)」，以 Tabs 展示并显式切换，支持无配置时先浏览再引导注册游戏 | **已实现** | `modules/ui-search/ui/index.ts` + `workbench-behavior.test.ts` |
| S05 | 搜索结果图片/头像本地缓存 | `main+dev` | `CacheApi.cacheImage` 将缩略图与作者头像缓存在本地磁盘并以 `convertFileSrc` 渲染 | 在线搜索卡片直接 `<img src={item.logoUrl}>` 加载 HTTPS 远程图（受 CSP `img-src 'self' https: data: blob:` 控制），无作者头像展示与本地图片磁盘缓存 | **实现但有差异** | `modules/ui-search/ui/index.ts` |
| S06 | 搜索结果下载量/订阅数与一键翻译/恢复原文 | `main+dev` | 卡片展示下载量、订阅数；右键「翻译成当前语言」（走 `api.v1st.net` 代理调 Google Translate）与「恢复」 | 搜索卡片仅展示名称、简介、来源标、remoteGameKey 与 `#remoteModId`；**无**下载量/订阅数展示，**无**翻译与恢复功能 | **未实现** | `modules/ui-search/ui/index.ts` |
| S07 | 在线 URL 粘贴批量添加 Mod（`OnlineTab`） | `main+dev` | 独立 `AddModDialog` 窗口支持一次粘贴多行 mod.io / Nexus / ModCat URL 批量解析添加并指定分组 | **未实现 UI 入口**：后端三个 source provider 均实现了 `parse_url` 单元测试，但 `commands.json` 无按 URL 添加命令，前端无粘贴 URL 批量添加弹窗，只能从搜索结果逐个点「添加」 | **仅后端/契约已实现** | `modules/source-*/src/lib.rs` + `commands.json` |
| S08 | 剪贴板监听自动弹出添加窗口 | `main+dev` | `ClipboardApi` 每秒轮询剪贴板，识别到合法 Mod 链接自动弹出 `AddModDialog`；`dev` 在设置页提供开关 | **未实现**：已彻底移除剪贴板轮询与设置开关 | **未实现** | `apps/desktop` grep `clipboard` 无结果 |
| S09 | mod.io 已订阅 Mod 一键同步导入（`SubscribedTab`） | `dev` | `AddModDialog` 第 3 个标签页拉取用户全部 mod.io 订阅列表，支持全选/反选批量导入 | **未实现**：`modules/source-modio` 无 `/me/subscribed` 接口，UI 无订阅导入页 | **未实现** | `modules/source-modio` grep `subscribed` 无结果 |
| S10 | 本地文件/目录添加（`.pak` / `.zip` / 未打包 `Content` 目录） | `main+dev` | `LocalTab` 支持系统文件/目录选择器与窗口拖拽（`tauri://file-drop`）多选添加，校验 `.pak`/`.zip`/未打包目录 | **实现但有差异**：`mods.importLocal` + `modules/source-local` 支持文件（`.pak`/`.zip`/`.js`/原生二进制）与目录不可变快照（严格防 symlink 逃逸、ZIP 路径穿越与压缩炸弹，并标记可执行风险）；但 **UI 无原生文件选择器与文件拖放**，发起后需在待审批卡片中手动输入绝对路径再点击「确认选择」 | **实现但有差异** | `modules/source-local` 7 项测试 + `library-view.tsx` |
| S11 | 搜索结果无限滚动与本地二次筛选 | `main+dev` | `InfiniteScroll` 滚动自动加载下一页 | 改为底部「加载更多结果」按钮手动触发游标翻页；新增「筛选已加载结果」前端即时过滤输入框 | **实现但有差异** | `modules/ui-search/ui/index.ts` |

---

### 2.6 Mod 下载、版本切换与更新检查

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| D01 | 单个 Mod 版本切换与历史版本固定 | `main+dev` | 点击下拉实时拉取远端文件列表，切换后写 `usedVersion` 并立刻触发下载；`dev` 用组件内锁防连点冲突 | `mods.get` 读取已知版本，`mods.setVersion` 支持 `auto`（自动最新）与 `pinned:<versionId>`（锁定带锁图标），生成 `prepare-and-apply` 事务任务，支持多版本并存且不把旧版本漂移成 latest；**差异**：仅当 `mods.get` 库内已有版本元数据时可切换，不会在打开下拉框时实时查远端新版本 | **实现但有差异** | `modules/library/ui/library.ts` + `mintcat-runtime-downloads` 9 项测试 |
| D02 | 多 Profile 共享下载、断点续传与校验 | `dev` | `src-tauri/src/download/` 实现并发下载、断点续传、MD5 校验、重试；但以 `modId` 覆盖全局 `cachePath`，易产生下载与编辑竞态 | **显著增强已实现**：`runtime/transfers` 分离文件身份/attempt/consumer，支持强 `ETag`/`Last-Modified` + `206 Content-Range` 续传、远端变更自动截断、下载→校验→发布严格分阶段、SHA-256 内容寻址去重、多 Profile 共享单取消不影响另一方 | **已实现** | `mintcat-runtime-transfers` 7 项测试 |
| D03 | 下载期间列表并发编辑保护 | `dev`（有缺陷） | 旧版进度事件携带整份 `CompleteModData` 易覆盖新名称/启停状态，且编辑与下载无互斥 | **显著增强已实现**：Library 单写者事务内持有 Profile-content lease 与单调 fence，`profiles.contentState` 暴露 `writable` 能力与独立 transfer overlay；下载锁定内容编辑但仍允许搜索、切 Profile、改共享名称；取消先撤销提交权防迟到回调污染 | **已实现** | `mintcat-library-service` + `mintcat-runtime-downloads` + `library-model.test.ts` |
| D04 | 在线检查 Mod 更新与过期/下架提醒 | `main+dev` | `CheckModUpdateTask`（启动 120s 自动查 + 手动按钮）：拉 mod.io events、ModCat 批量版本、Nexus 元数据，在列表标出时钟（有新版本）与红色叹号（已下架/本地缺失） | **未实现**：`commands.json` 中没有主动批量检查更新或刷新在线元数据的命令；工具栏「刷新列表」仅重读本地 Library 数据库；UI 列表无「发现新版本」「作者已删除」告警图标 | **未实现** | `commands.json` + `library.ts` `refresh` |
| D05 | 批量并发下载更新（`batchDownloadModFiles`） | `dev` | 多选后点击「更新」并发 3 路刷新元数据并下载；安装前自动补齐缺失缓存 | 后端 `runtime/downloads` 支持多 item 原子批下载（部分失败保留已缓存 artifact 且回滚 Profile 绑定），但 UI 仅提供单 Mod 切版本触发下载，无多选「批量更新」入口 | **实现但有差异** | `mintcat-runtime-downloads` 测试 + `library-view.tsx` |
| D06 | 下载进度展示与任务管理 | `main+dev` | TreeView 行内圆形进度条 + 底部状态栏进度条，刷新或关窗进度丢失 | 模组行内以 pending 圆点/Tooltip 展示待生效版本与百分比，并升级为独立「下载管理」侧栏页（`/mintcat.library/downloads`）与折叠任务面板展示所有持久后台任务与取消按钮，关窗后 Daemon 继续执行 | **实现但有差异** | `library-view.tsx` + `workbench-behavior.test.ts` |
| D07 | 自定义缓存目录与清除当前/旧版缓存 | `main+dev` | 设置页可查看/打开/修改缓存目录（修改时清空 modio 缓存路径），一键清除旧版 MINT 缓存（`main`）或当前全部缓存（`dev`） | 缓存由 Daemon 隔离 data-dir 下的 `artifacts/` 托管，提供 `updates.gc` 契约进行安全引用计数垃圾回收（保护活跃任务、收据、Library 绑定与回滚租约）；**无**自定义缓存目录 UI 和「清除缓存」按钮 | **实现但有差异** | `commands.json` `updates.gc` + `settings-overview.ts` |

---

### 2.7 Mod 打包、安装、冲突检测与卸载

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| I01 | DRG PAK 整合与蓝图/AssetRegistry 补丁 | `main+dev` | `PakIntegrator`：合并多 PAK/ZIP/未打包目录、注入 `MI_SpawnMods`/`PCB`/`Sandbox` 蓝图补丁、序列化 `AssetRegistry.bin`、部署 `x3daudio1_7.dll` | 算法完整迁移至 `modules/game-drg/src/integrator` + `modules/engine-unreal-pak`；改为隔离 staging 构建输出不可变 PAK 工件并重读校验，不直接写游戏目录；**阻塞项**：需外部提供签名 `resources/drg-patch.zip` 才可生成完整 DeployPlan | **实现但有差异** | `mintcat-game-drg` 4 项测试 + `mintcat-engine-unreal-pak` 5 项测试 + `mintcat-install` 8 项测试 |
| I02 | Rogue Core PAK 整合与零输入清理 | `dev` | `drgrc/pak_integrator.rs` 生成 `RogueCore-Windows_Mods.pak` + 集成 `RC.zip`；遗留零 Mod 清理异常 | 算法完整迁移至 `modules/game-rogue-core/src/integrator`；支持代表性构建与**零输入干净重建**（彻底修复缺失 Mod 残留）；**阻塞项**：需外部提供签名 `resources/rogue-core-patch.zip` | **实现但有差异** | `mintcat-game-rogue-core` 7 项测试 + `mintcat-install` 测试 |
| I03 | Mod Pak Zlib 压缩开关 | `dev` | 全局设置 `compressModPak`（布尔开关） | 每游戏独立设置 `compression`（`"none"` / `"zlib"`）（`modules/engine-unreal-pak` + `algorithms/unreal-pak` 实现确定性 V11 未压缩/Zlib 写入，明确拒绝未批准 Oodle 构建） | **已实现** | `mintcat-engine-unreal-pak` 测试 + `game-drg`/`game-rogue-core` descriptor |
| I04 | UE4SS 框架部署、JS Mod、Native DLL 与音频直拷 | `main+dev` | `main` 含 UE4SS-Lite/Custom 模式、内置 DLL/框架与 `ue4ss/mods/<name>/main.dll`；`dev` 改为外部 `UE4SSL.zip` + 启用/禁用开关 + JS Mod（`js/main.js`）+ ≥10MB 音频 PAK 直拷 + RC UE 5.6 `UE4SS-settings.ini` 钩子配置 | `modules/loader-ue4ss` 完整实现签名 UE4SS 资源绑定、DRG/RC 差异化 `UE4SS-settings.ini` 生成、JS ZIP/目录精准提取 `js/`、PE DLL 校验部署 `main.dll`、音频 PAK/ZIP 校验部署 `*_P.pak` 并生成可执行代码审批要求；**阻塞项**：需签名 `resource.ue4ss` 资源包 | **已实现** | `mintcat-loader-ue4ss` 8 项测试 |
| I05 | C# Mod 支持（`UE4SSL.CSharp.dll` / `csmods`）与 .NET Runtime 安装 | `main+dev` | `main` commit `f9d4d31` 部署 `UE4SSL.CSharp.dll`/`Framework`/`Runtime` 与 `csmods` 目录；`dev` commit `9a2ce3b` 曾加 .NET Runtime 下载安装（后续改为随 `UE4SSL.zip` 分发） | `modules/loader-ue4ss` 未显式提供 `csmods` C# Mod 规划路径或 .NET Runtime 下载安装能力；若要支持 C# 框架需通过签名 `resource.ue4ss` 资源映射，独立 C# Mod 部署规则未单列 | **未实现** | `modules/loader-ue4ss/src/lib.rs` 核对 |
| I06 | 原子安装事务、旧安装接管与安全卸载 | `main+dev` | 安装前**先卸载旧文件**再现场构建（构建失败会破坏原可用安装）；卸载按约定目录暴力删除 `ue4ss/` 等；旧 MINT `mods_P.pak` 弹窗询问卸载 | **显著增强已实现**：`runtime/file-transactions` + `modules/install` 严格区分 `install.plan`（隔离构建 + 校验）→ 人工审批 → `install.apply`（物理目录锁 + 同卷 staging + 备份 + journal + `COMMITTED` marker + 收据）；旧无收据安装走精确摘要接管审批；`install.uninstall` 按收据链逆序恢复原始字节，绝不误删用户修改文件、第三方 loader 或外来音频 PAK | **已实现** | `mintcat-runtime-file-transactions` 16 项测试 + `mintcat-install` 8 项测试 |
| I07 | 游戏运行中进程检查 | `main+dev` | `main` 的 `check_steam_game` 首行直接 `return false`（失效）；`dev` 检查硬编码 `FSD.exe`（根 TODO 已知 bug，RC 失效） | **已修复实现**：按各游戏变体描述声明精确进程名（DRG `FSD-Win64-Shipping.exe`/`FSD-WinGDK-Shipping.exe`/`FSD.exe`；RC `RogueCore-Win64-Shipping.exe`/`RogueCore.exe`），使用 `sysinfo` 检测并阻止运行中写入 | **已实现** | `mintcat-game-drg`、`mintcat-game-rogue-core`、`mintcat-runtime-file-transactions` 测试 |
| I08 | 安装前检测游戏 Paks 目录外来 `.pak` 文件弹窗确认 | `dev` | `ModInstallTask` Step 1 调 `checkForeignPaksInPaksDir`，若发现外来 `.pak` 弹 `ForeignPaksConfirmContent` 确认继续 | 新版 `detect_legacy_install` 会识别旧 MintCat/MINT 接管文件并将外来 `.pak`/第三方 loader 标为 foreign 予以保留，但**无**「发现外来 `.pak` 弹窗警告用户是否继续安装」的交互 | **实现但有差异** | `modules/game-drg/src/plan.rs`、`modules/install/src/service.rs` |
| I09 | Mod 文件冲突检测（独立任务与列表标红） | `dev` | 工具栏「检查冲突」提交 `ModConflictCheckTask`，调后端 `check_mod_conflicts` 并在 TreeView 对应 Mod 上展示冲突文件数与冲突对象 | **部分实现/缺失独立 UI**：`modules/engine-unreal-pak` 与 `modules/install` 在 `mods.pack` / `install.plan` 阶段计算并输出 first-wins `conflicts` 摘要，但 `commands.json` 无独立冲突检查命令，模组管理页无「检查冲突」按钮与行内冲突提示 | **实现但有差异** | `modules/engine-unreal-pak/src/lib.rs`、`modules/install/src/validation.rs` |
| I10 | 安装页面 UX 差异（手动填写输入格式与收据 ID） | `main+dev` | 旧版一键点击「保存更改」或「卸载 Mod」自动判断格式并完成 | 新版安装页（`modules/install/ui/index.ts`）要求：1) 为每个启用 Mod 手动下拉选择「准确输入格式（`pak`/`zip`/`directory-snapshot`）」与勾选「仅音频提示」；2) 卸载需手动填写「已验证收据 ID」和「绑定的配置版本」；3) 审批时展示原始 JSON `window.confirm` | **实现但有差异** | `modules/install/ui/index.ts` |

---

### 2.8 账号、OAuth 凭据与支持者（VIP）体系

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| U01 | 用户设置弹窗与头像/用户信息展示 | `main+dev` | 标题栏头像打开 `UserSettingDialog`（`main` 仅在设置页填 mod.io key + 弹窗看用户名/ID；`dev` 升级为统一头像弹窗显示 mod.io 头像、用户名、UID、4 平台密钥标签页） | **未实现**：新版无头像入口、无用户信息拉取、无 OAuth/API Key 管理界面 | **未实现** | `apps/desktop/src/chrome.ts`、`settings-overview.ts` |
| U02 | 各平台 OAuth / API Key 录入与安全存储 | `main+dev` | 旧版将 MintCat / mod.io / ModCat / Nexus 令牌明文存 SQLite `oauths` 表（`dev` 仅 Agent API Key 存 OS keyring） | **后端安全升级，前端缺失**：`runtime/broker` 实现 `OsCredentialStore`（OS keyring）且 Library 绝不存储明文 token（仅存 `CredentialRef` 引用）；旧库导入时可勾选迁移凭据入 vault；但**无 UI/CLI 命令用于日常增删改凭据**（需通过 `runtime.json` 引用已有 `credentialRef`） | **仅后端/契约已实现** | `runtime/broker/src/credential.rs` + `commands.json` |
| U03 | MintCat VIP 支持者状态校验与徽章 | `dev` | 调 MintCat API 验证支持者状态（Active/Expired/None）与到期时间，展示皇冠徽章与 `vip.mintcat.work` 链接 | **未实现**：新版完全移除 VIP 状态检测逻辑与相关 UI | **未实现** | 仓库全局检索无 VIP 逻辑 |
| U04 | 401 鉴权失败快捷引导跳转用户管理 | `dev` | 监听 `modio-unauthorized` 弹出带「前往用户管理」链接的错误提示 | 搜索页实现了中文友好错误提示组件（`presentUiError`），区分鉴权失败并允许手动切换其他可用来源，但因无用户设置页故无一键跳转录入密钥入口 | **实现但有差异** | `modules/ui-search/ui/index.ts` + `packages/sdk-ui/src/error-presentation.ts` |

---

### 2.9 备份、云同步与旧版配置迁移

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| B01 | 云端备份列表、上传、下载与恢复 | `dev` | `CloudBackupApi` + `CloudBackupSettings` + 标题栏一键云同步按钮；直接上传/下载 `mintcat.sqlite` 并在下次重启替换文件 | 完整实现 `backup.cloud.list` / `backup.cloud.upload` / `backup.cloud.download` 契约与 UI；改为逻辑快照传输 + 人工审批恢复；**差异**：1) 默认本地应用未内置云端 endpoint，调用返回 `CLOUD_PREREQUISITE`（需在 `runtime.json` 的 `cloud` 字段配置 HTTPS 端点与 `credentialRef`）；2) **未暴露 `backup.cloud.delete` 命令**（后端 `cloud.rs` 有 `delete` 方法但未进入 `commands.json` 与 UI）；3) 标题栏无一键云同步按钮 | **实现但有差异** | CLI 实测 `CLOUD_PREREQUISITE` + `modules/feature-backup/src/cloud.rs` + `commands.json` |
| B02 | 旧版配置自动发现与导入（v0.2 / v0.3 / v0.4 JSON & v0.6 SQLite） | `main+dev` | `ConfigManageDialog` 自动扫描本机旧版 MINT 0.2/0.3/0.4 JSON 配置目录，支持勾选导入、打开目录、删除旧配置文件夹 | **实现但有差异**：新版仅支持显式传入路径导入 **v0.6 SQLite 数据库**（`library.import-legacy`）或 **format2 便携归档 / SQLite 快照**（`library.config-import`），通过只读事务保留 WAL 写入与全部映射，需人工审批；**不再自动扫描/导入 v0.2/v0.3/v0.4 的 JSON 配置文件**，也不支持删除旧配置目录 | **实现但有差异** | CLI 实测导入 `fixtures/baseline/legacy/mintcat-v0.6.sqlite` + `modules/feature-backup/ui/view.ts` |
| B03 | 配置目录查看与一键打开 | `main+dev` | 设置页展示当前配置目录路径并点击「打开」调系统文件管理器 | 仅在「后台状态与恢复（Rescue）」面板显示 `Isolated vNext data` 路径，无「打开文件夹」按钮 | **实现但有差异** | `apps/desktop/src/bootstrap.ts` |

---

### 2.10 Agent 助手与外部 CLI

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| AG01 | 侧边栏 Agent 对话抽屉与工具调用 | `dev` | `AgentPanel` 侧滑抽屉，内置 12 个工具（查 active profile/folder/mod、搜索、添加、改名、启停、切版本、移动、删除）；模型调用变更工具时前端弹窗二次确认；对话仅存内存，关窗/刷新丢失 | `modules/feature-agent` 侧滑 540px 抽屉：从同一 `CommandSpec` 自动生成工具（当前暴露 32 条 `agentExposed: true` 命令），会话持久化于独立 SQLite，回合作为 Daemon 持久 Job 运行（关页面不中断）；模型不可自行批准 `actions.approve`/`install.apply`/`install.uninstall`；**差异**：创建会话需显式填写 **6 个冻结目标字段**（`gameId`, `profileId`, `profileRevision`, `contentRevision`, `installationId`, `installationRevision`），使用体验门槛高于旧版自动取当前配置 | **实现但有差异** | `mintcat-feature-agent` 7 项测试 + CLI `agent sessions create/delete` 实测 |
| AG02 | Agent 模型设置页与连接测试 | `dev` | `SettingPage/AgentSettings.tsx` 独立配置 `baseUrl`、`model`、`apiKey`（存 OS keyring，含清除按钮与 HTTP 明文警告），提供「测试连接」按钮 | 无独立设置页卡片，配置折叠在 Agent 抽屉顶部表单中且仅接受 `credentialRef`（不直接输入 API Key），无「测试连接」按钮与清除密钥入口 | **实现但有差异** | `modules/feature-agent/ui/index.ts` |
| AG03 | 外部自动化 CLI（`mintcat-cli` → `mintcat`） | `dev`（草稿设计 + 未提交桥接） | 旧版设计（`docs/design/agent-cli.md`）依赖 GUI 运行中的 loopback TCP bridge，仅支持 `status/tools/call/confirm/reject` 5 个命令，无法脱离桌面运行 | **显著增强已实现**：独立产品二进制 `mintcat` 直连独立 `mintcatd`（完全无需 GUI），基于 68 条契约自动生成全部领域子命令（`games/profiles/groups/mods/sources/install/uninstall/jobs/actions/updates/backup/agent/proxy/settings/modules/api/completion/environment`），支持 `--json`、`--wait`、`--events`、`--all` 分页、退出码分级与 Shell 自动补全；**注意**：实测发现 `mintcat status` 会触发 `connect_or_start` 自动建目录并拉起 Daemon（与 README 文档声明不一致） | **已实现**（含 1 处行为偏差） | `mintcat-cli` 7 项测试 + CLI 全链路实测 |

---

### 2.11 网络代理与加速（Proxy）

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| N01 | 网络代理设置与系统代理自动检测 | `dev` | `NetworkSettings` 支持配置 HTTP 代理地址（如 `http://127.0.0.1:7890`）、切换资源节点（自动/大陆/国际）+「测试连接」测速延迟 + MintCat 反代模式（自动/强制开启/关闭）；Windows 注册表自动探测系统代理 | 后端 `runtime/broker` 支持显式 Direct / Forward 代理路由策略（且隔离凭据不泄露给代理），但**无「网络设置」页面**、无 HTTP 代理输入框、无资源节点测速切换、无 Windows 系统代理自动读取 | **仅后端/契约已实现** | `mintcat-runtime-broker` 7 项测试 + `apps/desktop/src/settings-overview.ts` |
| N02 | Steam 加速代理（`mintcat-proxy` / `AcceleratorPage`） | `dev` | `AcceleratorPage` 独立页面（菜单中默认注释）：下载安装代理插件、安装根证书、启停 443 端口加速、计时球体动效、查看运行日志 | **已按产品决策停止默认接入（R25 取消）**：`modules/feature-proxy` 保留完整独立实现与 `proxy.*` 4 条契约，仅在编译 `mintcat-daemon` 时启用 `--features experimental-proxy` 可用；默认构建返回 `MODULE_UNAVAILABLE`，桌面端无加速器页面，亦不修改系统证书/hosts | **已取消（保留实验代码）** | 默认 CLI 实测返回 `MODULE_UNAVAILABLE`；`mintcat-module-feature-proxy` 5 项测试通过；`mintcat-daemon --features experimental-proxy` 6 项通过 |

---

### 2.12 热更新与发行打包体系

| 编号 | 功能点 | 来源基线 | 旧版本实现细节 | 当前分支现状与对比 | 状态 | 验证依据 |
| --- | --- | --- | --- | --- | --- | --- |
| R01 | 整应用自动更新弹窗（`tauri-plugin-updater`） | `main+dev` | 启动时调用 `@tauri-apps/plugin-updater` 检查 `latest.json`（GitHub / OSS），弹出 `UpdateDialog` 展示 Markdown 更新日志并下载安装 | **未实现**：桌面端已移除 `tauri-plugin-updater` 与 `UpdateDialog` | **未实现** | `apps/desktop/src-tauri/Cargo.toml` |
| R02 | 前端资源、内置资产与后端运行库热更新（含发布渠道切换） | `dev` | `StartupUpdateCheckTask` 轮询 OSS `update.json` + 设置页切换 `alpha`/`beta`/`stable` 渠道，自动下载前端 ZIP（`mintcathot://`）、`mintcat_backend_runtime.dll` 与 `DRG.zip`/`RC.zip`/`UE4SSL.zip`（无真实公钥校验） | **架构重构**：官方 UI 与内置业务服务随主应用一起编译发布，取消独立前端热更新、单 DLL 替换与未签名资产自动拉取；改为 `runtime/updates` + `runtime/release` 基于 **TUF 风格 Ed25519 真实验签** 的**仅游戏插件按 Scope 独立更新、渠道授权（`updates.channel`）、Probation 观察、确认、局部回退与 Safe Mode/Rescue 恢复**（UI 暂无发布渠道切换控件） | **实现但有差异** | `mintcat-runtime-release` 15 项 + `mintcat-runtime-updates` 12 项测试 + `commands.json` |
| R03 | Windows 标准安装包（NSIS）/ 便携版 / Fixed WebView2 打包 | `main+dev` | `scripts/build-release.sh`（NSIS）、`build-portable.mjs`、`build-fixed-webview2.mjs` + `webview2.x64.json` | 统一收口到 `scripts/package-release.mjs`（支持 `standard` / `portable` / `fixed-webview2` / `cli` 四种变体）+ 新增 `scripts/build-app.mjs`（本地 macOS ad-hoc `.app`）；强制要求先经过 `mintcat-module stage` 验签，拒绝开发服务器 GUI 与未签包；**当前仅在 macOS 上有完整实跑证据，Windows 目标机矩阵尚未实机验收** | **实现但有差异** | `scripts/build-app.test.mjs` 通过 + 脚本核对 |

---

## 3. 实测记录与运行证据

以下为本轮在 `feat/modular-runtime-glass-ui`（`f3b7f1d`，macOS `aarch64-apple-darwin`）实际执行的全部测试与运行验证结果：

### 3.1 自动化测试与静态检查

| 检查/测试项 | 执行命令 | 实测结果 |
| --- | --- | --- |
| 契约生成一致性 | `pnpm contracts:check` | **通过**：68 条 `CommandSpec` 与 Rust/TS 生成产物完全同步 |
| TypeScript 全量类型检查 | `pnpm -r --if-present check` | **通过**：`contracts`、`sdk-ts`、`sdk-ui`、`desktop-vnext`、`feature-agent` 全部 0 错 |
| 桌面端行为与 UI 单测 | `pnpm --filter @mintcat/desktop-vnext test` | **通过**：5 files / 18 tests passed（含无插件浏览、双游戏独立设置草稿、离线备份导航持久化、拖拽排序边界、Safe mode 恢复） |
| 共享 UI SDK 测试 | `pnpm --filter @mintcat/sdk-ui test` | **通过**：3 files / 15 tests passed |
| TS SDK 契约边界测试 | `pnpm --filter @mintcat/sdk-ts test` | **通过**：1 file / 9 tests passed |
| 打包脚本测试 | `pnpm test:scripts` | **通过**：1 test passed |
| 桌面端生产构建 | `pnpm --filter @mintcat/desktop-vnext build` | **通过**：3142 modules，`dist/index.html` + `assets/workbench-*.js` + `assets/index-*.js`（2.72 MB） |
| Rust 全工作区测试（默认功能） | `cargo test --workspace --exclude mintcat-module-feature-proxy` | **通过**：**225 passed, 0 failed**（覆盖 CLI、Daemon、Desktop 连接器、Library、Unreal PAK、UE4SS Loader、DRG、RC、Install、Agent、Backup、mod.io/ModCat/Nexus/Local、Transfers、Downloads、File Transactions、IPC、Jobs、Kernel、Release、Supervisor、Updates 等全部 crate；含 1 条无害 unused import warning） |
| 实验性 Proxy 模块测试 | `cargo test -p mintcat-module-feature-proxy` | **通过**：**5 passed, 0 failed**（含离线 TLS 转发、租约保持与撤销、端口冲突清理） |
| Daemon 启用 Proxy 特性测试 | `cargo test -p mintcat-daemon --features experimental-proxy --lib` | **通过**：**6 passed, 0 failed** |
| 按 Scope 更新专项测试 | `cargo test -p mintcat-runtime-updates --test scoped_updates` | **通过**：**11 passed, 0 failed** |

### 3.2 真实进程端到端冒烟测试（CLI + Daemon + 签名游戏 Worker + 受控 HTTP 源/LLM）

在完全隔离的临时目录（测试后已全部停止进程并清理）中执行了覆盖未配置默认态、旧库迁移态与完整配置态的端到端真实进程验证：

1. **目录隔离、启动边界与 Shell 补全**：
   - `mintcat --data-dir <nonexistent> --json environment`：返回 `initialized: false`，确认**未创建**目录。
   - `mintcatd --data-dir <unmarked-dir-with-mintcat.sqlite> init`：退出码 `1`，明确拒绝初始化包含旧库 `mintcat.sqlite` 的未标记目录。
   - `mintcat completion zsh`：退出码 `0`，生成 251,838 字节包含 `#compdef mintcat` 的完整补全脚本。
   - **发现偏差**：`mintcat --data-dir /tmp/mc-status-check status` 在目录不存在时返回 `exit 0`、`ready: true` 且**自动创建了目录并拉起了 `mintcatd`**（因 `apps/cli/src/main.rs` 统一走 `connect_or_start`），与 `README.md`「`status` 不初始化存储或启动 daemon」及桌面端 `daemon_status` 的行为不一致。
   - `mintcat doctor`：返回 `healthy: true`，4 项检查全部 `ok`（`runtime-layout`、`data-directory-write`、`endpoint-permissions`、`peer-authentication`）。
2. **默认 Builtin 状态核验（无 `runtime.json`）**：
   - `modules.list` 返回 `0` 个模块，`sources.list` 返回 `[]`，`games.list` 返回 `0` 个游戏。
   - 无游戏插件时对 `game:drg` 读取 `settings.get` 成功，但执行 `settings.update` 返回退出码 `5` / `MODULE_UNAVAILABLE`。
3. **旧版数据库无损导入与规范化（`library.import-legacy`）**：
   - 对 `fixtures/baseline/legacy/mintcat-v0.6.sqlite` 发起导入：`jobs watch` 返回退出码 `3`（`ACTION_REQUIRED`），`actions get` 返回 `backup-owner-import`，人工 `actions approve` 后任务完成（`succeeded`）：
     - 导入 2 游戏（`game:drg` 与由 `rc` 规范化迁移而来的 `game:rogue-core`）、2 配置、3 分组、3 Mod、4 版本、3 关联、2 设置、2 条 `legacyInstallHints`、0 条明文凭据，并输出 8 条告警（旧缓存路径与 `installedHash` 仅作 hint，不冒充已验证 artifact 或 receipt）。
4. **全量 Library CRUD、乐观锁、幂等性与树保护（无 GUI 实测）**：
   - **Profile**：`profiles.create` 同幂等键重放返回同一 `profileId`、同键异参返回 `CONFLICT`；`profiles.update` 过期修订返回退出码 `4` / `REVISION_CONFLICT`；`profiles.activate`（强制校验显式 `gameId`）、`profiles.clone`、`profiles.delete` 均通过。
   - **Group**：`groups.create`（根组与嵌套子组）、`groups.update`（改名与收起 `expanded: false`）、`groups.move` 移动父组进子组被循环检测拦截（`INVALID_ARGUMENT`）、`groups.delete` 删除最后 1 个分组被拦截（退出码 `2` / `INVALID_ARGUMENT`：`a profile must retain at least one group`）。
   - **Mod**：`mods.list --page-size 1 --all` 游标翻页（2 页）、`--query` 筛选、`mods.get` 版本详情、`mods.setEnabled`、`mods.setEnabledBatch`、`mods.rename --scope shared`、`mods.move`、`mods.cleanupMissing`、`mods.remove`（实测克隆 Profile 删除关联后原 Profile 条目不受影响）全部通过。
5. **离线备份往返、非空拒绝、覆盖拒绝与维护恢复**：
   - `backup.export` 产出 `formatVersion: 2` 逻辑归档（`ownerSchemas: { "mintcat.library": 5, "mintcat.feature.agent": 1 }`，`credentialPolicy: "references-only-reauthenticate-on-import"`）。
   - 对已存在路径再次 `backup.export` 拒绝覆盖（`CONFLICT` / `recovery-required`）；对全新空目录执行 `backup.restore` + 人工审批后成功还原全部 2 游戏与全部 Mod；对非空目录再次 `backup.restore` 同步拒绝（退出码 `4` / `CONFLICT` / `library.not_empty`）；`library.config-import --format backup` 审批恢复通过；`backup.recover` 执行通过。
   - 默认环境下 `backup.cloud.list` 返回退出码 `1` / `CLOUD_PREREQUISITE`；`proxy.status` 返回退出码 `5` / `MODULE_UNAVAILABLE`。
6. **完整配置态实测（签名 DRG 插件 + 受控本地 HTTP 源 & Mock OpenAI 服务）**：
   - 以临时测试 Ed25519 密钥签发真实 `mintcat-game-drg-worker` 并发布到本地 TUF 仓库，通过 `MINTCAT_RUNTIME_CONFIG` 启动 Daemon：`modules.list` 激活 `mintcat.game.drg`，`sources.list` 激活 `modcat`/`modio`/`nexusmods`。
   - **每游戏设置校验**：`settings.update` 成功写入 `{ compression: "zlib", ue4ssEnabled: false }`（revision `0 -> 1`），传入不支持枚举 `compression: "oodle"` 被拒绝（`INVALID_ARGUMENT`）。
   - **三源在线搜索**：`sources.search` 实测 ModCat 返回 `Loopback ModCat Mod`（且空 `PicUrl` 规范化省略 `logoUrl`）、mod.io 返回 `Loopback Modio Mod`；Nexus Mods 在未配置 `credentialRef` 时安全 fail-closed 返回 `SOURCE_UNAUTHORIZED`（`credential_missing_or_invalid`）。
   - **在线添加 + 真实 HTTP 下载 + 原子应用**：对 ModCat 与 mod.io 依次执行 `mods.add` + `mods.setVersion (prepare-and-apply)`，均真实经 loopback HTTP 拉取 `baseline.pak` 字节、校验并发布 SHA-256 artifact，原子生效 `appliedVersionId`（`mc-ver-1` / `file:9001`）并推进 `contentRevision`。
   - **本地导入全格式 + 拒绝流程**：依次对真实 `baseline.pak`、`baseline.zip`、`directory-basic/` 执行 `mods.importLocal` -> `awaiting-approval` -> `actions.approve` -> `succeeded`；对第四次选择执行 `actions.reject`，任务以 `failed` / `INTERACTION_REJECTED` 结束。
   - **游戏目录发现与安全 GC**：对合成 DRG 目录执行 `games.discover` 成功识别 `steam` 变体（1 条候选）；执行 `updates.gc` 正确保留 3 个活跃 Mod 内容 blob 与 1 个活跃 DRG 模块包（`removed: []`）。
   - **真实 Agent 回合与越权审批拦截**：向本地 Mock OpenAI 发起 `agent.sessions.send`，模型第 1 轮调用 `mods__list` 成功获取真实模组列表，第 2 轮尝试调用人类专属命令 `actions__approve` 被 Daemon 明确拒绝（`{"ok":false,"error":{"code":"PERMISSION_DENIED","message":"agent authority denied"}}`），第 3 轮模型输出最终回复，任务 `succeeded`。
7. **官方游戏插件校验与真实双游戏 Worker 冒烟**：
   - 直接校验官方插件目录 `target/debug/mintcat-module check modules/game-drg` 与 `modules/game-rogue-core`：两者均成功编译 worker 并在资源检查阶段明确拒绝：
     - `release rejected: bundled resource archive resources/drg-patch.zip missing from signed game payload`
     - `release rejected: bundled resource archive resources/rogue-core-patch.zip missing from signed game payload`
   - 运行 `node tools/module-dev/smoke-games.mjs`（使用 `TEST-ONLY` 标记归档）：
     - `game:drg` 与 `game:rogue-core` 两个真实独立 Worker 均通过：签名构建、TUF 仓库打包/验签、生产 Supervisor 握手与健康检查、`describe`、`discover`、合成主 PAK 验证、`build.plan`、`build.prepare`、跨游戏 scope 拒绝、未授权 artifact 拒绝、伪造签名拒绝、未签名路径拒绝，且在生成输出与部署前正确拦截了 `TEST-ONLY-NOT-INSTALLABLE.txt` 标记归档。

### 3.3 当前环境未覆盖（外部条件受限）

以下项目因缺少外部授权资产或目标平台环境，未在本次实测中执行，均不计为通过：
- Windows 10/11 实机运行（DWM Acrylic、Named Pipe ACL、游戏文件独占锁、NSIS 安装器 / Portable ZIP / Fixed WebView2 启动）；
- 真实正版 DRG / Rogue Core 游戏主 PAK 与真实签名 `drg-patch.zip` / `rogue-core-patch.zip` / `UE4SS.zip` 的端到端安装与进游戏验证；
- 真实生产账号下的 mod.io / ModCat / Nexus Mods（含网页登录）/ OpenAI / 云备份服务端联调。

---

## 4. 当前分支相对旧版的新增核心能力

为便于评估升级收益，单列当前分支超出 `origin/main` 与 `dev` 的新增能力：

1. **独立后台守护进程（`mintcatd`）与认证本地 IPC**：业务与 SQLite 单写者完全移出渲染进程，关闭桌面窗口不会中止下载、备份、安装或 Agent 任务。
2. **统一契约事实源（68 条命令）**：`packages/contracts` 单点生成 Rust SDK、TS SDK、CLI 子命令及 Agent 工具 schema。
3. **无 GUI 全功能产品 CLI（`mintcat`）**：脱离桌面独立执行查询、编辑、导入、打包、任务跟踪、审批、备份与 Shell 补全。
4. **持久 Jobs / Actions 与显式人工审批机制**：任何高风险操作（安装部署、旧安装接管、卸载、备份恢复、旧库导入、本地文件授权、Nexus 浏览下载）均具持久化审批记录，绑定精确目标、修订号、计划哈希与模块锁，Agent 模型无法越权批准。
5. **内容寻址 Artifact Store 与引用计数 GC**：不可变 SHA-256 工件 + 租约保护，杜绝下载覆盖正在使用的缓存或误删活跃收据依赖。
6. **下载期间的原子能力门禁与 Fencing**：`prepare-and-apply` / `cache-only` 分离，彻底消除旧版下载进度与列表编辑竞态覆盖问题。
7. **独立「仅打包，不部署（`mods.pack`）」用例**：可先产出并验证整合 PAK 与冲突报告而不写入游戏目录。
8. **Journal + Receipt 原子游戏文件事务**：同卷 staging、物理根目录锁、逐文件备份与 `COMMITTED` 标记，失败回滚不破坏上次可用安装，卸载仅还原收据自有且未被用户修改的文件。
9. **各游戏独立签名插件与按 Scope 热更新/回退**：DRG 与 Rogue Core 各自独立构建、Ed25519/TUF 验签、Probation 观察与局部回退，互不耦合。
10. **离线便携归档（`format2`）与维护状态恢复（`backup.recover`）**：无云端服务器亦可跨机迁移完整 Library 与 Agent 数据，排除明文凭据与运行权限。
11. **桌面端原生毛玻璃材质与无障碍响应**：macOS `UnderWindowBackground` 与 Windows 11 DWM Acrylic，自动响应「减弱透明度」。
12. **桌面端独立 Rescue（诊断与恢复）与 Safe Mode**：主工作台加载异常时仍可独立查看 Daemon 状态、日志、插件清单及执行本地已确认集合回退。
13. **严格最小权限桌面安全边界**：撤销渲染进程通用 SQL/FS/Shell/Dialog 权限并收紧 CSP，所有业务仅通过 `daemon_call` IPC 执行。
14. **每游戏独立设置与精确游戏进程检测**：解决旧版根 `TODO` 记录的三项历史问题（RC 清理缺失 Mod、每游戏独立打包设置、启动检查固定 `FSD.exe`）。

---

## 5. 后续补齐建议（按用户感知优先级）

| 优先级 | 缺口/问题 | 建议处理方式 |
| --- | --- | --- |
| **P0（阻断真实可用）** | 1. 缺少签名游戏补丁资源（`drg-patch.zip`/`rogue-core-patch.zip`/`UE4SS.zip`）<br>2. 默认无 `runtime.json` 导致无在线源与无游戏插件<br>3. 无 UI 凭据录入入口（mod.io / ModCat / Nexus / OpenAI / Cloud） | 补齐受签游戏资源包与默认内置源配置；在设置中心增加经 `runtime/broker` 写入 OS Keychain 的凭据管理页面（只展示脱敏状态与 `credentialRef`，不暴露明文） |
| **P0（行为 Bug）** | CLI `mintcat status` 自动建目录并拉起 Daemon，与文档及桌面端行为不一致 | 在 `apps/cli/src/main.rs` 中将 `system.status` 改为仅使用 `IpcClient::connect`（不调 `connect_or_start`），未运行时直接返回非零状态且不建目录 |
| **P1（核心体验回归）** | 1. 一键启动游戏<br>2. 粘贴 URL 批量添加 Mod & 剪贴板监控<br>3. 本地导入与游戏路径选择改回系统原生文件选择器 / 拖放<br>4. 在线检查 Mod 更新 & 过期提醒 & 批量更新<br>5. 独立冲突检测入口与安装页自动推断输入格式/收据列表 | 1) 通过受控白名单命令支持已注册游戏启动；<br>2) 暴露 `sources.resolveUrl` / URL 批量添加入口；<br>3) 通过宿主 broker 桥接原生文件/目录选择器完成 `select-file`/`select-directory` Action；<br>4) 新增 `mods.checkUpdates` 命令；<br>5) 安装页自动读取 Library 已记录的 format 与 active receipt 列表，免除手工填 ID |
| **P2（交互完善）** | 1. 中英文 i18n 切换<br>2. Mod 列表标签/审批/来源组合筛选、按时间/认证排序、复制链接/列表<br>3. 搜索页排序、下载量/订阅数、Nexus 受控 WebView 弹窗<br>4. Agent 自动带入当前选中游戏/配置/安装目标（免手动填 6 个 ID/修订号）<br>5. 云备份删除命令（`backup.cloud.delete`）与 C# Mod（`csmods`）支持评估 | 按需在现有契约体系上补齐命令与前端交互，不回退单写者与审批安全边界 |
