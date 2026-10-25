## 模块化重构（工作区）

### 2026-10-25：新增完整 macOS app 打包脚本

- 新增 `scripts/build-app.mjs` 和 `pnpm package:app`：构建前端及 Desktop、CLI、daemon、模块验证器，生成二进制 digest、immutable stage 和 assembly plan，复用现有验证器与 assembler，仅输出本地 ad-hoc 签名 `.app`，并检查最终签名和内嵌入口。
- 默认读取已有完整 `MintCat.app` 的签名基线，也支持通过 `--baseline` 指定 stage 或 Resources；保留模块版本、信任根和来源配置，不读取签名私钥、不清空数据、不隐式激活新 scope。业务模块变更须显式提供新签名基线。
- 输出默认使用时间戳目录；`--output` 只接受新目录，拒绝覆盖已有产物或写入输入 app／基线。缺少基线会在构建前报错，避免再次交付仅有桌面主程序的包。
- 验证：新命令完整构建 `target/app-script-smoke/assembled/bundles/macos/MintCat.app`；实际原生 WebKit 在独立测试目录加载已签名 Library 页面且未进入恢复模式，后台诊断 `healthy: true`。已验证帮助、缺失基线、已有输出和基线内部输出的错误边界；使用说明已加入 README。

### 2026-10-25：补齐本地 app 运行时并验证启动

- 修正本次构建交付中仅封装桌面主程序的不完整 `.app`：实际启动在创建窗口前报 `Packaged signed baseline runtime configuration is missing`。没有移除或绕过签名基线检查。
- 补编 CLI、daemon 和模块验证器，从现有本地缓存恢复 13 个已签名模块，保留信任根、模块版本和已确认 scope 内容；通过完整 immutable stage 与标准 assembler 打包运行时配置、模块基线、信任资源及配套可执行文件。未清空或迁移本地数据，未修改应用版本或生产更新清单。
- 已替换 `target/release/bundle/macos/MintCat.app`，完整组装产物位于 `target/mac-app-startup-fixed/assembled/bundles`。验证实际原生窗口启动、WebKit 中已签名在线模组页挂载和后台连接、`modules.list` 及 `doctor` 的 `healthy: true`，并保存原生 UI 快照；不是仅凭 CodeSign 或内嵌资源自检判断可运行。本地包采用 ad-hoc 签名，未进行 Apple 公证。

### 2026-10-10：来源初始化与用户报错提示修正

- 定位仍报错的真实本地运行状态：活跃 platform 仍为第一包，搜索页和来源 worker 没有随应用替换而切换；mod.io / Nexus 构造器又把下载域名白名单误当成浏览前提。只读搜索、详情和版本查询不再要求配置下载域名，解析和宿主验证仍拒绝所有未获准的二进制下载。
- 原生 worker 保留错误类别和安全原因，`INVALID_ARGUMENT` 不再被包装成来源不可用；鉴权、限流、配置、返回数据及网络错误分开处理。worker / daemon 的诊断使用静态原因，不反射来源响应、凭据或 URL；保留请求关联。
- 新增共享 UI 错误呈现，搜索页和宿主复用中文原因及操作提示。技术错误码、消息键与请求标识默认折叠，错误不再与“没有找到模组”混用；配置读取失败结束加载，过期请求不覆盖用户手动切换后的状态。仅提供已启用、已绑定来源的显式切换，不自动回退。
- 当前本地包未配置 mod.io / Nexus 凭据。真实 mod.io 请求返回 `SOURCE_UNAUTHORIZED / credential_missing_or_invalid`；界面说明需要有效凭据，并允许手动切换 ModCat。没有虚构密钥、绕过鉴权或把授权失败伪装成成功。
- 重签搜索 UI `0.1.2`、mod.io / Nexus `1.0.1`、ModCat `1.0.2`；应用版本保留 `0.1.0`，最终 platform 序号 4、元数据版本 6，游戏 scope 及其余 9 个模块保留。旧界面留下的 7 个过期会话导致首次升级准备受阻：先正常回滚准备、释放维护锁，再关闭已退出会话并发布新的签名候选；没有伪造 UI 快照、清空库或清除发布隔离记录。用户本地目录已实际确认最终候选并选中四个新模块。
- 验证：共享 UI / Desktop 类型检查、17 项前端回归、source / daemon / supervisor 31 项库回归、3 项真实 worker 集成、ModCat 5 项及显式 `--features fixtures` IPC lifecycle 4 项通过。最终 `.app` 签名、嵌入资源与 DMG 校验通过；最终 DMG 内原生 WebKit 验证手动切换 ModCat 展示 50 条真实结果，Nexus 缺少凭据显示授权提示。隔离旧库的 Profile ID、revision、contentRevision 不变，原生跨未登记游戏切换恢复原配置且继续展示 50 条结果。升级入口在屏障阻塞时正常回滚并保留配置，重复已确认升级不新增操作；证据保存在 `target/mac-source-errors-fixed/smoke-evidence`。本地包为 ad-hoc 签名、未公证，生产更新清单未修改。

### 2026-10-10：在线模组修复的 macOS 本地重打包

- 重建 Apple Silicon 原生桌面、ModCat worker 与 daemon；应用版本保留 `0.1.0`，搜索 UI 签发为 `0.1.1`，ModCat 签发为 `1.0.1`，platform 发布序号提升到 2、元数据版本到 4，两款游戏 scope 与其余 11 个模块保持不变。复用包外 `0600` 本地签名私钥和现有信任根，继续使用 `com.mint.cat.vnext.local` 隔离数据身份。
- 保留数据的真实签名升级另复现：`updates.activate` 已持久化 probation，却因刷新来源路由重新申请 Service admission 而失败，错误为 `maintenance lock held`。platform probation 期间保留上一套已确认的原生路由，待 `updates.confirm` 完成健康检查、打开 owner 写入并释放迁移锁后再发布新路由；未提前释放锁、跳过确认或放宽 UI 屏障。
- 本地包提供显式 `activate-local-platform.mjs --yes` 升级入口：检查声明数据目录的 daemon 锁和可执行文件，必要时正常重启旧后台，再执行已签名 platform 的 stage/prepare/activate/confirm；不清空 Library、不伪造 UI 快照或销毁确认。仅覆盖 `.app` 不会自动替换已登记的活跃 scope。
- 最终 `.app` 的 deep/strict CodeSign、离线嵌入入口、DMG 校验和签名 stage 均通过；daemon 5 项库测试通过。最终 DMG 内实际原生 WebKit 验证未登记游戏无初始化错误，并通过真正的已签名 worker/daemon 展示 50 条 ModCat 结果；无图片模组保持在结果中。隔离旧库副本通过交付升级入口正常切换，已有 Profile 的 ID、revision 与 contentRevision 完全保留；原生界面跨未登记游戏切换后恢复配置，继续展示 50 条结果。重复升级入口不产生额外升级；本轮未修改用户原库或生产更新清单，macOS 本地包采用 ad-hoc 签名、未公证。

### 2026-10-09：在线模组初始化与搜索结果修正

- 修复在线模组页首次启动和选择未登记游戏时的 `mintcat.ui.search: library.not_found`：先通过 `games.list` 查询 Library 登记，仅对已登记游戏调用 `profiles.list`。已签名游戏支持与 Library 登记不再混为一谈；已有配置的选择、分页和跨游戏切换仍保留。
- 未登记游戏仍可浏览已签名来源；在注册真实安装目录并创建配置前，禁用目标配置和添加操作，显示不会被搜索结果状态覆盖的引导。配置加载失败结束加载状态，过期游戏查询的成功或失败不会覆盖当前选择。
- Application API 失败由搜索页就地展示原因、错误码和请求 ID，不再把整段 AppError JSON 当作提示，也不重复上抛到宿主显示未翻译的错误键；失败时不误显示“没有找到模组”。
- 真实 ModCat 首屏另复现 `INVALID_RESULT / $.result.items[10].logoUrl`：来源返回空图片地址导致整页违反非空契约。搜索和详情解析将缺省、null、空字符串及纯空白图片规范化为 `None`，canonical 搜索结果省略缺失的 `logoUrl`；不删除模组、不缩小分页、不放宽契约。
- 验证 shared SDK / Desktop TypeScript 检查与 ModCat 5 项回归；新增缺失图片回归修复前失败、修复后通过。Chromium 中当前构建的搜索 UI 使用真实 daemon 验证未登记、已有配置及跨游戏切换；新版 provider 的只读直连冒烟完整返回并展示 50 条真实结果，其中 6 条没有图片。受控异步查询验证加载失败、过期查询成功/失败及登记变化后的 generation 刷新；本轮未重签或重打包原生应用，未注册游戏或修改原库。

### 2026-10-09：macOS 本地应用打包

- 修正桌面原生图标配置：移除指向已删除旧目录的 `icons/icon.ico` 软链接，改用当前 `apps/desktop/src/assets/icon.ico`，避免 Tauri 因缺失图标中止 `.app` / DMG 生成。
- development macOS assembly 显式使用 ad-hoc bundle identity，确保 `Contents/Resources` 与 sidecar 一起写入 CodeResources；不再生成“可执行文件有签名、应用资源未封装”的无效本地包。生产签名与公证仍要求 Apple Developer 身份和凭据。
- 用户提供的新包 UUID 与红色关闭按钮崩溃报告推翻了此前的关闭验收结论：`NSWindow.performClose:` 调试调用不能替代真实 `mouseDown` / `mouseUp`。原生鼠标事件配合 NSZombie 追踪确认失效对象为 `WryWebViewParent`，不是旧包或仅调试清理时的问题。
- 修正 macOS 材质同步的 content view 所有权：在主线程同步回调内借用 `contentView` 的 +0 返回值，不生成会在回调结束时释放该视图的 owning wrapper。修复前追踪到两次材质同步对应的额外 release，窗口关闭时在 autorelease pool 中触发 `SIGSEGV`；修复后重复切换材质保持窗口内容视图的引用平衡。
- 删除此前用于绕过崩溃的原生 `NSWindow` 保留表、手动清空 content view 和解挂 WebView 操作，将窗口层级的销毁交还 Tauri / Wry / AppKit。`CloseRequested` 与应用级 `RunEvent::Exit` 仅释放辅助功能监听器。
- 修复首次启动和切换未注册游戏时的 `mintcat.library: library.not_found`：先查询 Library 中的游戏登记，仅对已登记游戏加载配置；未登记游戏进入真实安装目录注册引导，不再把它误报为数据库丢失。游戏切换或模块 generation 变化后的旧查询结果不会覆盖当前选择。
- 按用户选择重新签发本地模块基线并隔离数据：development 应用标识改为 `com.mint.cat.vnext.local`，macOS release 默认使用 `~/Library/Application Support/com.mint.cat.vnext.local/vnext`，不读取或修改旧 `com.mint.cat.vnext.dev` 与正式版数据。新的模块签名私钥以 `0600` 权限保存在包外，后续更新复用，不能再当构建临时文件删除。
- 已验证 TypeScript 检查与 8 项 Library 模型测试；最终原生包中两款未注册游戏均显示安装引导，隔离测试库中的已有配置可加载并在跨游戏切换后恢复；36 次材质切换后分别通过真实红色按钮鼠标事件和菜单 Quit 正常退出。

### 2026-10-01：毛玻璃遮挡修正

- 用户整屏截图显示前一版仍像实色灰白面板。检查实际窗口确认 NSWindow / WKWebView 已透明、`drawsBackground=false`、降低透明度未启用，但 Light 根染色 58% 与页面面板 28% 叠成约 70% 遮罩；仅检查材质 API 状态不足以验收真实透色。
- macOS 全窗材质由 Sidebar 切换为 `UnderWindowBackground / BehindWindow`；Light 根 / 面板染色降至 18% / 16%，Dark 降至 32% / 14%，基础叠层约为 31% / 42%（不含系统材质、高光与交替行）。保留文字和整个窗口 `opacity=1`，表头、菜单继续使用更实的背景。
- 前端生产构建、原生 custom-protocol 构建与嵌入入口检查通过；实际预览窗口确认材质为 21、透明原生背景和新 CSS 染色，Light / Dark 切换保留 68 条数据并恢复 Light。真实桌面合成截图仍受屏幕录制授权限制，未把这些属性检查或 WebKit 内容截图宣称为可见透色验收；保留当前预览窗口供实屏检查。

### 2026-10-01：浅色 / 深色原生毛玻璃 UI

- macOS 使用真实 Overlay 标题栏、系统窗口按钮和 `NSVisualEffectView` Sidebar / BehindWindow 材质；根背景改为乳白 / 蓝灰半透明染色，不添加模拟壁纸、不降低文字与图标整体透明度。
- 原生材质跟随保存的 Light / Dark 主题；串行合并主题与透明度变化，成功后才启用透明根背景。支持 `prefers-reduced-transparency` 和 macOS 系统辅助功能通知，关闭页面 / 窗口时释放监听器；降低透明度或材质不可用时回到实色。
- 统一轻玻璃工具栏、面板、粉色主操作 / 筛选胶囊 / 开关、圆角选中行和元数据色签；类别与认证仍来自真实 tags / approvalStatus，未知值保持 `—`。粘性表头与浮层使用更实的背景，表格行不重复 blur；主题颜色即时切换，保留阴影 / 位移动效。
- Windows 11 22H2+ 配置公共 DWM Acrylic 并检查 HRESULT；更旧 Windows 与 Linux 使用实色回退。Windows 效果未在本轮实机验证。macOS 透明 WKWebView 需要 Tauri private API，不适用于 Mac App Store 分发。
- `pnpm check`（workspace Rust / TypeScript / 契约）、桌面生产构建、原生 custom-protocol 构建与嵌入离线入口验证通过。隔离签名基线使用当前 `mintcat.game-adapter@2` worker 与脱敏原库副本，真实 DRG 68 条正常显示；验证搜索、深 / 浅主题切换时输入保留、选中行、材质清除 / 恢复及 960×620 窗口下工具栏可达、表格横向滚动和粘性表头。原生浅 / 深色截图与计算样式确认主操作文字、侧栏文字 / 图标不滞留旧主题。未修改原库或迁移凭据。
- 截图是实际 WebKit 内容，不包含系统标题栏或桌面合成；窗口材质、appearance、透明背景与系统按钮由 AppKit 分别检查，不把内容截图当作桌面透视或物理鼠标操作的验收。

### 2026-10-01：原库数据与原生窗口

- 去掉参考图外部桌面背景、窗口外边距和 HTML 三颗窗口按钮。macOS 使用真实系统 decorations/Transparent 标题栏；内容区 1500×900，logo 位于左上角 `(14,16)`，不再为假窗口按钮留空。
- 在独立 daemon/签名模块中导入原库的只读一致副本：2 个游戏、2 个 Profile、232 个 Mod、84 条 Profile membership、303 条版本记录。真实 DRG 68 条、Rogue Core 16 条；原数据库与 WAL hash 不变，凭据只在副本中脱敏，实际迁移凭据为 0。
- Library 合约与 UI 使用真实 tags、approvalStatus、单一 currentVersionId；展示类别/多人要求/认证，未知游戏版本为 `—`。旧库 sentinel 和歧义 current 不显示为真实版本，缓存路径不冒充已验证产物或安装 receipt。
- 实际 native UI 验证搜索、开关关闭/恢复、两游戏切换、Profile 克隆 68 条/重命名/删除验证副本，以及安装路径设置页。本地文件/目录选择经 UI 取消后，持久任务返回 `INTERACTION_REJECTED`，未导入未选择文件。
- 按产品决定停止继续接入 Proxy，保留实验实现；证书/hosts/高权限/该实验许可不再是当前验收前置条件。修正旧库导入/便携备份对未准入 Proxy 的隐式依赖：仅省略从未写入的空 owner；已有 Proxy 状态仍拒绝静默遗漏，批准/schema/epoch 围栏不变。
- 8 个签名包/7 个 UI 的隔离 native baseline 完成真实导入、无 Proxy 离线备份与遗留状态拒绝冒烟。Light/Dark 内容截图已验证并发布；系统按钮另由公开 AppKit API 验证，内容截图不伪称包含系统标题栏。证据：`/tmp/mintcat-original-data-ui.ngOCtt/native-real-data-ui-evidence.json`。
- `pnpm check`、Library 22 项、桌面 11 项、daemon lib 5 项通过；IPC lifecycle 显式 `--features fixtures` 4/4，通过 native custom-protocol desktop/daemon/CLI/Library worker 构建。未开放生产 fixture gate，也不宣称真实游戏资源/安装、Windows 发布或生产凭据迁移完成。
- 修正 Library 拖动排序：根据目标行上/下半区插入到前/后，按移除拖动项后的完整同组成员计算位置；分组普通拖动调整同级顺序，只有 Shift 拖动才移入目标分组，并显示插入线/嵌套落点。来源分组仅允许同来源、同实际配置分组排序；跨配置分组移动通过“配置分组”视图，不再偷偷改变隐藏分组。
- 拖拽回归修复前2项失败，修复后桌面3文件16项与 `pnpm check` 通过。使用原库副本、独立签名 Library 0.1.2 验证原生 WebKit 拖拽事件、canonical 写入、防循环嵌套及后台重启/原生窗口重开后的顺序保留；原库未修改。证据：`/tmp/mintcat-drag-order-proof.zJy2VQ/native-drag-order-evidence.json`，不将标准事件注入冒充物理鼠标拖动验证。
- 清理已退役的根目录 `src/`、`src-tauri/`，删除约14GB旧编译缓存及重复图标；保留 `apps/desktop/src`、`apps/desktop/src-tauri` 和 `public`。唯一旧 `oo2core_9_win64.dll` 已归档到仓库外 `/Users/bytedance/Project/mintcat-legacy-assets.x5vNEx/`，通过逐字节比较与 SHA-256 一致性检查，不作为已批准的运行时 codec。
- 移除旧 WebView2 路径的 ignore 项和失效源码链接；历史文档明确标记已移除实现，固定 WebView2 打包说明切换到签名 stage/assembly plan 的当前入口。清理后 `pnpm check`、Desktop生产构建、原生 custom-protocol 构建与 `--verify-embedded-ui` 通过；重新构建的真实 native 窗口与 canonical API 均证明原库副本 DRG 68条/全部启用、contentRevision8不变。证据：`/tmp/mintcat-legacy-cleanup-proof.YEas8h/cleanup-evidence.json`；未修改生产数据库或当前签名模块。
- 将前端 Logo 从根目录 `public/icon.ico` 移到 `apps/desktop/src/assets/icon.ico`，通过显式 `?url` 导入交给 Vite 指纹打包；README 改为引用应用内资产，`publicDir: false`，删除根目录 `public/` 及4个未引用旧主题 CSS。不覆盖原生 bundle 图标，也不保留旧 `/icon.ico` 兼容资源。
- Logo 原文件、迁移文件、生产资产及开发页面实际加载字节的 SHA-256 一致。开发页面与重新构建的 native 窗口实际显示正常；native 从嵌入的 `/assets/icon-DMk7PdSg.ico` 解码256×256原图，布局不变。`pnpm check`、Desktop生产构建、native custom-protocol构建和离线入口验证通过；证据：`/tmp/mintcat-desktop-assets-proof.Xrng9Q/asset-relocation-evidence.json`。
- 将 DRG 与 Rogue Core 专属安装算法分别移入 `modules/game-drg/src/integrator`、`modules/game-rogue-core/src/integrator`；RC UE5.6 UE4SS 策略与行为回归随插件迁移。共享 `mintcat-integrator-core` 删除游戏模块/export/builder feature 及废弃发现路径，仅保留通用工具；两款插件分别构建，正常依赖图互不包含。
- 通用 `WorkerGameAdapter` 按游戏签名 manifest 声明绑定固定闭包 provider，删除强制 Unreal 与宿主 UE4SS 规划；两款游戏 worker 的 `build.prepare` 复用共享 Unreal/loader 能力，prepared 文件经 output grant 校验发布，安装审批、文件事务、journal 与 receipt 仍共享。worker 契约切换为 `mintcat.game-adapter@2`，service/UI 与 scope locks 需重新签名发布，不混装旧 `@1` 基线、不保留兼容别名；不新增或宣称 Unity 安装器。
- 拆分相关回归51项、daemon lib5项、显式 fixtures 的 IPC lifecycle4项通过；真实签名 DRG/RC worker 验证 first-wins 输出字节、UE4SS/JS 制品 grant、脚本审批、禁用 loader 拒绝及不写游戏根，受控 manifest 验证没有隐式 Unreal 绑定。证据：`/tmp/mintcat-game-algorithm-split-proof.3pOiZy/worker-evidence.json`；合成 fixture 不替代真实 Windows 游戏/资源版本矩阵。

### 此前模块化实施记录

- 将产品入口切换为独立 `mintcat` CLI、`mintcatd` 和 contribution 驱动的桌面框架；删除旧 renderer CLI bridge、FFI 安装器和前端业务数据库写入路径。
- 游戏/Unreal PAK 引擎通过签名包、固定 digest 闭包和独立 worker 调用；安装使用不可变 plan、持久批准、文件 journal 和 receipt。
- 使用统一 `mintcat-module` 开发及签名发布工作流；已完成隔离本地 `new/dev/check/pack/compose/verify/publish` 与原生 CLI staging 演练。
- Agent 已用真实 native owner、受控 HTTP 模型和统一 Application API 验证持久会话、目标绑定、断连查询、取消及独立人工审批；模型不能自行批准 action，canonical 错误结果不再含非法 null details。
- 逻辑备份升级为 format2：Library/Agent/Proxy owner 快照、签名版本与 schema、epoch 批准围栏和显式恢复；不转移凭据、任务/审批、outbox/lease、安装 receipt、Proxy 私钥/hosts 或缓存文件。已实际验证多 owner 往返及进程中断后不重放恢复。
- 修复备份检查持有共享 Kernel 锁后调用其他 owner 导致的死锁；本地签名 stage 的验签/哈希/复制移入 blocking pool，真实六包 staging 时单线程 executor 的 timer 仍持续运行。
- Rust/TS SDK 对 canonical nullable result 增加严格 null 类型支持；旧 `rc` 游戏标识只在 legacy 导入边界迁为 `rogue-core`，不新增 API 别名。
- 桌面发行要求实际嵌入离线入口：`custom-protocol` 构建和 `--verify-embedded-ui` 检查，assembler 拒绝依赖开发服务器的 GUI 工件。
- 修复 scope 确认状态未参与 generation 缓存键导致的新任务继续使用旧插件；实际 DRG/engine 更新、RC 独立更新及局部回退的 native 打包闭包已验证。
- GC 合并 Library/transfer 与 prepared/applying/active receipt 的 module/input/deploy 引用；修复 GC 后缓存 Mod 输入被删除导致的重打包失败，两个游戏 GC 后均已重新打包并验证产物 hash。
- 真实 APFS 磁盘满只拒绝当前更新，不隔离合法签名版本；清除 I/O 故障后可重试同一 release。已实际运行 error 28 且 active scope 不变。
- 发行采用唯一验签 staging、显式原生 verifier 与独立权限/hash pin；只读 DMG 消费者不再创建 publisher lock，解压 CLI 与只读 `.app` 内 CLI 在无开发环境变量时均启动完整签名离线基线。
- 按后续深/浅参考图重写模块化 UI，替代此前沿用旧样式的方向：172px 文字侧栏、72px 顶部工具栏、毛玻璃圆角框架、紧凑来源分组表格和统一粉色强调；本地导入/下载管理为真实 Library 页面，搜索工具栏通过共享 React portal 挂入宿主槽，未知元数据不造假。现有主动作仍为真实“安装模组”，不伪造“启动游戏”API。
- UI 模块共用宿主 React/ReactDOM/Ant Design 单实例；初始 React commit 同步纳入挂载验收，后续渲染错误隔离，普通 API 操作失败不卸载页面。修复原生 bridge 固定 5 秒预算拒绝实际 5.83 秒合法模块查询的问题，使用统一 120 秒 Application envelope deadline，不增加重试或降低验签。
- 原生并发实跑复现 `modules.list` 同步校验签名包耗时 6.49 秒、饿死 IPC hello 的独立 5 秒握手预算；将验签 inventory/序列化放到 blocking worker，并在 worker 完成前保留 `UiCallAdmission` fence。真实 9 包 inventory 期间六条并发握手均在 0–1ms 接受，未延长握手、删 hash 校验或增加重试。
- 独立 module-dev 宿主按已签名 page route 只挂一个页面，页面导航/重载保留 scoped state；真实 worker 的两页面切换与旧页清除已运行，不再对同一容器挂全部 page exports。
- 深/浅主题隔离 CSS variable palette key，并同步提交主题事件而不 remount 页面；实际 Chromium 的三个共享 React/Ant Design root 与跨 DOM portal 验证选择器两套文字颜色、已编辑 draft 保留和无渲染错误。
- macOS 14+ 原生工作台用公开 BackgroundThrottlingPolicy::Disabled 保持后台 lease heartbeat/更新 barrier 应答；真实隐藏 WebKit 默认 suspend 曾造成 session 失效，不修改可见性、动画或期限。深/浅原生图使用真实保存偏好、签名 UI 和 canonical 导入数据；前台激活受当前运行环境限制，隐藏 CSS 过渡不冒充前台验证。
- 更新 prepare 使用唯一 native StateOwner drain/一致备份/迁移/health 流程，删除对 stateless supervisor 重复排空 native owner 的旧路径；durable update 生命周期的同步验签/owner 工作不再阻塞 IPC executor。
- UI barrier 的 READY/FAILED 对该 session 完成投递，其他 participant 与 Probation gate 仍保留；修复 READY 后重复 dispose/remount 导致页面状态反复卸载，增加两客户端、重启与确认门禁的行为回归。
- Library native owner 的 probation health 建立经过当前 schema/integrity 检查的只读连接，让已发布 UI 可查询真实 Profile/设置；不重开 writer、不迁移旧库、不借 reader 允许变更。8 个 native owner 行为回归及实际签名 UI probation 查询通过。
- 修复同一路由更新时仍被 paused 标记挡住、等待页却被确认 READY 的问题：只挂精确匹配已发布新 generation 的候选页，挂载成功后才 READY，保持 context 与后端写入围栏。真实原生窗口不重启即恢复 v2 页面的输入草稿/SAVED，同 participant 恰好一次 Disposed/Ready，最终 confirmed。
- 在线模组样式适配 Ant Design 6 的实际 Select DOM，统一 12px 字号、共享 10px 圆角和玻璃边框；真实签名 Light 控件通过。当前环境的隐藏原生 Dark CSS transition 停在首帧，目标样式/签名 bytes 均正确，但未将该中间帧记为 settled Dark 视觉验收。
- Proxy 已通过 canonical Application 的独立 CA 批准、真实 CA 校验 TLS 转发、停用 UI 后保留 service identity、原版本 stop 与占用端口失败清理；未修改系统证书或 hosts，系统授权与再分发许可仍未验收。
- 验收仍以 `docs/design/modular-refactor-todo.md` 的逐项记录为准；不将本地测试根、合法合成游戏输入或 macOS 演练记为生产/Windows 发布。

**Full Changelog**: https://github.com/iris-cat-dev/mintcat/compare/v0.5.3...v0.5.5

下载地址 / Download：https://github.com/iris-cat-dev/mintcat/releases

# 中文

## 新增

1. 新增框架（Framework）设置，可以禁用 UE4SS 提高部分掉帧玩家体验。
2. 新增前端资源与安装器运行时更新机制，支持独立热更新前端和 runtime。
3. 前端 Mod 列表新增序号方便用户定位 Mod。
4. 新增侧边栏 Agent 助手，支持通过 OpenAI 兼容模型查询、在线搜索和确认后管理当前 Profile 的 Mod。
5. 新增面向外部 AI agent 的 `mintcat-cli`，支持工具发现、结构化 JSON 输出和变更操作二次确认。

## 优化

1. 优化 “Buyable Mission” Mod 缺少选项的问题。
2. 优化暗色主题显示不明显问题。
3. 优化滚动条不明显问题。
4. 优化 TreeView 版本选择交互与样式展示。
5. 优化搜索框，支持动态过滤与更清晰的搜索交互。
6. 优化下载流程，补充断点续传、重试、缓存校验和下载状态更新。
7. 优化分组删除、在线 Mod 不可用、下载失败等场景的错误提示。
8. 优化状态栏、标题栏、菜单栏和引导流程中的部分 UI 表现。
9. 优化本地化文案，统一中英文提示与错误信息。
10. 优化请求日志脱敏，减少 token、鉴权参数等敏感信息暴露风险。
11. 优化 Mod 批量启用和批量禁用。
12. 优化TreeView 文件夹排序能力。

## 修复

1. 修复切换 Profile、启停 Mod 或更新在线 Mod 后部分场景不会正确刷新 TreeView 状态的问题。
2. 修复在线 Mod 更新后版本、下载进度、缓存路径与更新时间戳不同步的问题。
3. 修复安装前本地缓存缺失或在线版本变化时未自动补齐下载的问题。
4. 修复部分分组删除和下载失败场景下错误处理不一致的问题。
5. 修复 RougueCore 启动失败问题。
6. 修复 Agent 无法读取当前 Profile 中已有 Mod 列表的问题。
7. 修复 Agent 请求未使用系统代理且网络错误缺少底层原因的问题。

## 重构

1. 重构 runtime 加载、校验、更新与资源路径处理，支持后续 DLL/独立运行时热更新。
2. 重构 Pak 资源集成、冲突序列化与音频 Pak 处理流程。
3. 重构网络层，新增 `NetworkClient`、路由策略、认证解析和统一请求类型。
4. 重构 ModMapper、资源处理和部分 ViewModel 初始化流程。

# English

## New

1. Add Framework settings, allowing UE4SS to be disabled to improve the experience for some players affected by frame drops.
2. Add frontend asset and installer runtime update mechanisms, supporting independent hot updates for the frontend and runtime.
3. Add a side-panel Agent assistant that uses OpenAI-compatible models to query, search, and manage active-profile mods after explicit confirmation.
4. Add `mintcat-cli` for external AI agents with tool discovery, structured JSON output, and two-step mutation confirmation.

## Improvements

1. Improve missing options for the “Buyable Mission” mod.
2. Improve low-contrast display issues in the dark theme.
3. Improve scrollbar visibility.
4. Improve TreeView version selection interaction and styling.
5. Improve SearchBox with dynamic filtering and clearer search behavior.
6. Improve the download flow with resume support, retries, cache validation, and download status updates.
7. Improve error messages for group deletion, unavailable online mods, and download failures.
8. Improve selected UI details in the status bar, title bar, menu bar, and onboarding flow.
9. Improve localization strings and align Chinese/English prompts and errors.
10. Improve request log sanitization to reduce exposure of tokens and authentication parameters.
11. Improve batch enable and disable actions for mods.
12. Improve TreeView folder ordering support.

## Fixes

1. Fix TreeView state not refreshing correctly after profile switches, mod enable/disable actions, or online mod updates in some cases.
2. Fix version, download progress, cache path, and timestamp data not staying in sync after online mod updates.
3. Fix missing automatic downloads before installation when local cache is missing or the online version has changed.
4. Fix inconsistent error handling for some group deletion and download failure cases.
5. Fix RogueCore startup failure.
6. Fix the Agent failing to read the populated Mod list in the active profile.
7. Fix Agent requests not using the system proxy and network errors hiding their underlying cause.

## Refactoring

1. Refactor runtime loading, validation, update handling, and asset paths to support future DLL/standalone runtime hot updates.
2. Refactor Pak asset integration, conflict serialization, and audio Pak handling.
3. Refactor the network layer with `NetworkClient`, route policies, auth resolving, and unified request types.
4. Refactor ModMapper, asset handling, and parts of ViewModel initialization.
