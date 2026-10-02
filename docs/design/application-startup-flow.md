# MintCat 应用启动流程

> 历史文档：旧前端初始化/SQL/任务队列启动链已切除，根目录 `src/`、`src-tauri/` 及旧编译缓存已清理；下文旧路径和代码仅保留历史记录，不是可用入口。当前流程为独立 daemon 认证 IPC、验签离线 scope、只读接入实际 owner、恢复维护/文件事务，再开放业务写入；桌面只挂载签名 contribution 或独立 rescue。当前边界见 [模块化运行时架构](modular-runtime-architecture.md)，实际验收见 [执行记录](modular-refactor-todo.md)。

本文档详细描述了 MintCat 应用从启动到完全初始化的完整流程。

## 目录

- [启动流程概览](#启动流程概览)
- [详细启动阶段](#详细启动阶段)
- [关键文件索引](#关键文件索引)
- [启动流程图](#启动流程图)

## 启动流程概览

MintCat 是一个基于 Tauri 2.0 + React 的桌面应用，启动流程分为以下主要阶段:

1. **Rust 后端启动** - Tauri 应用初始化
2. **前端资源加载** - HTML/JS 资源加载
3. **日志系统初始化** - 全局日志和错误处理
4. **React 应用启动** - 主题、国际化、路由配置
5. **依赖注入注册** - ViewModels IoC 容器注册
6. **核心初始化** - 数据库、迁移、AppViewModel
7. **UI 渲染** - 应用界面呈现

## 详细启动阶段

### 阶段 1: Rust 后端启动

**入口文件**: `src-tauri/src/main.rs:4`

```rust
fn main() {
    mintcat_lib::run()
}
```

**主要逻辑**: `src-tauri/src/lib.rs:19`

1. **错误追踪初始化** (L20-31)
   - 初始化 Sentry 客户端
   - 配置 minidump 崩溃报告

2. **Tauri Builder 配置** (L34-75)
   - 窗口事件处理 (主窗口关闭时退出应用)
   - 加载 Tauri 插件:
     - `tauri-plugin-store`: 键值存储
     - `tauri-plugin-clipboard-manager`: 剪贴板管理
     - `tauri-plugin-updater`: 自动更新
     - `tauri-plugin-opener`: 文件/URL 打开
     - `tauri-plugin-process`: 进程管理
     - `tauri-plugin-dialog`: 系统对话框
     - `tauri-plugin-shell`: Shell 命令
     - `tauri-plugin-log`: 日志系统
     - `tauri-plugin-os`: 操作系统信息
     - `tauri-plugin-fs`: 文件系统
     - `tauri-plugin-sql`: SQLite 数据库

3. **命令处理器注册** (L64-73)
   - 模组安装/卸载命令
   - Steam 游戏启动/检查
   - 文件下载命令
   - DevTools 开关命令

### 阶段 2: 前端资源加载

**入口文件**: `index.html:10`

```html
<div id="root"></div>
<script type="module" src="/src/main.tsx"></script>
```

浏览器加载 HTML 并执行 `main.tsx` 模块。

### 阶段 3: 日志系统初始化

**初始化代码**: `src/main.tsx:16`

```typescript
InitLog();
```

**日志 API**: `src/apis/LogApi.ts:4`

1. **控制台转发** (L6-28)
   - 拦截 `console.log/debug/info/warn/error`
   - 转发到 Tauri 日志插件持久化存储

2. **全局错误处理** (L44-78)
   - 捕获未处理的 Promise rejection
   - 捕获同步错误 (window.error)
   - 重写 `window.onerror` 处理器

### 阶段 4: React 应用启动

**主组件**: `src/main.tsx:19`

```typescript
const Main = () => {
    // 1. 主题系统初始化
    const defaultTheme = getDefaultTheme();
    const [theme, setTheme] = React.useState(defaultTheme);

    // 2. 主题变更监听
    useEventListener("theme-change", (themeValue) => {
        const defaultTheme = renderTheme(themeValue);
        setTheme(defaultTheme);
    });

    // 3. 开发模式配置
    useEffect(() => {
        if (import.meta.env.DEV) {
            EventDebugger.enable({...}); // 启用事件调试器
        }
        if (!packageJson.version.endsWith("dev")) {
            document.addEventListener('contextmenu', handler); // 禁用右键菜单
        }
    }, []);

    // 4. React 组件树
    return (
        <I18nextProvider i18n={i18n}>           {/* 国际化 */}
            <ConfigProvider theme={theme}>       {/* Ant Design 主题 */}
                <AntdApp>                        {/* Ant Design 全局配置 */}
                    <HashRouter>                 {/* 路由 */}
                        <Routes>
                            <Route path="/home" element={<App/>}/>
                            <Route path="/add_mod_dialog" element={<AddModDialog/>}/>
                        </Routes>
                    </HashRouter>
                </AntdApp>
            </ConfigProvider>
        </I18nextProvider>
    )
}

ReactDOM.createRoot(document.getElementById("root")).render(<Main/>);
```

### 阶段 5: 依赖注入注册

**App 组件**: `src/App.tsx:83`

```typescript
React.useEffect(() => {
    console.log('App 组件加载...');

    // Register all ViewModels to DI container
    registerViewModels();

    // Initialize core (database + AppViewModel)
    AppInitializer.initializeCore()...
}, []);
```

**ViewModel 注册**: `src/core/IoCRegistration.ts:22`

注册以下 ViewModels 到 IoC 容器 (懒加载，访问时才初始化):

- `AppViewModel` - 应用级状态和业务逻辑
- `TreeViewModel` - 模组树视图管理
- `HomeViewModel` - 主页状态管理
- `ProfileViewModel` - 配置文件管理

### 阶段 6: 核心初始化

**初始化器**: `src/core/AppInitializer.ts:53`

```typescript
static async initializeCore(): Promise<void> {
    // Phase 1: Database
    this.currentPhase = InitPhase.Database;
    await StorageAPI.getInstance();

    // Phase 2: Migration
    this.currentPhase = InitPhase.Migration;
    await MigrationBase.autoMigrate();

    // Phase 3: Core ViewModel
    this.currentPhase = InitPhase.CoreViewModel;
    await IoC.get(AppViewModel);

    this.currentPhase = InitPhase.Complete;
}
```

#### Phase 1: 数据库初始化

**Storage API**: `src/storage/index.ts:60`

1. **单例模式** (L60-85)
   - 检查是否已初始化
   - 等待进行中的初始化
   - 创建新实例并初始化数据库

2. **数据库初始化**: `src/storage/db/DatabaseInitializer.ts:11`

   ```typescript
   public static async initializeDatabase(): Promise<boolean> {
       if (!await this.checkTableExists("games")) {
           await this.enableForeignKeys();    // 启用外键约束
           await this.createTables();         // 创建表结构
           await this.setDefaultValues();     // 插入默认值
       }
       return true;
   }
   ```

3. **表结构创建** (L129-141)
   - 从 `ALL_SQL_CONTENT` 读取 SQL 语句
   - 按 `--> statement-breakpoint` 分割
   - 依次执行 CREATE TABLE 语句

4. **默认值插入** (L110-124)
   - 插入默认游戏: Deep Rock Galactic
   - 插入默认用户: default_user

#### Phase 2: 数据迁移

**迁移基类**: `src/storage/migration/MigrationBase.ts`

1. **检查迁移需求** (L42-54)
   - 查询数据库是否已有数据 (检查 mods 表)
   - 如果有数据则跳过迁移

2. **自动迁移流程**
   - 获取现有配置列表 (V2/V3/V4)
   - 选择最新版本配置
   - 迁移配置数据到数据库

3. **确保默认 Profile** (`DatabaseInitializer.ts:30`)
   - 检查 profiles 表是否为空
   - 创建默认 profile: "Default Profile"
   - 创建 mod.io 和 Local 文件夹

#### Phase 3: AppViewModel 初始化

**AppViewModel**: `src/AppViewModel.ts:138`

1. **单例获取** (L138-150)
   - 使用锁机制保证线程安全
   - 创建实例并调用 initialize()

2. **初始化流程** (L107-122)

   ```typescript
   protected async initialize(): Promise<void> {
       await this.loadUserLanguages();      // 加载用户语言设置
       await this.loadUserGuiTheme();       // 加载 GUI 主题
       await this.loadUserInfo();           // 加载用户信息
       await this.loadGameInfo();           // 加载游戏信息
       await this.checkAppPath();           // 检查应用路径
       await this.checkOauth();             // 检查 OAuth 状态
       await IntegrateApi.checkGamePath();  // 检查游戏路径

       await emitVoidEvent("title-bar-load-avatar");
       if (await DeviceApi.isFirstRun()) {
           await emitVoidEvent("config-manage-dialog-open");
       }

       this.initialized = true;
   }
   ```

3. **各子任务详情**:

   - **loadUserLanguages** (L64-73)
     - 从数据库读取语言设置
     - 如果为空则从系统获取
     - 切换 i18n 语言

   - **loadUserGuiTheme** (L75-83)
     - 从数据库读取主题设置
     - 默认为 "Light"
     - 触发 "theme-change" 事件

   - **loadUserInfo** (L85-91)
     - 获取活跃用户
     - 触发 "user-info-load-success" 事件

   - **loadGameInfo** (L93-100)
     - 获取活跃游戏
     - 触发 "game-info-load-success" 事件

   - **checkAppPath** (L40-56)
     - 检查缓存路径和配置路径
     - 如果不存在则设置为默认值

   - **checkOauth** (L29-38)
     - 检查 mod.io OAuth 状态
     - 如果已登录则启动自动更新检查 (120秒后)

### 阶段 7: UI 渲染

**App 组件**: `src/App.tsx:90`

```typescript
AppInitializer.initializeCore()
    .then(() => {
        console.log('[App] Core initialization complete');
        pageConfigs.current.push({key: MenuPage.Home, component: <HomePage/>});
        setIsAppViewModelReady(true);  // 触发 UI 渲染
    })
    .catch((error) => {
        console.error('[App] Core initialization failed:', error);
        emitEvent('app-error', error.message);
    });

initClipboardWatcher();  // 初始化剪贴板监听
```

**渲染逻辑** (L122-133)

```typescript
{
    !isAppViewModelReady && <EmptyPage/>  // 初始化中显示空页面
}
{
    isAppViewModelReady && pageConfigs.current.map(({key, component}) => (
        <div key={key} style={{display: currentPage === key ? 'block' : 'none'}}>
            {component}
        </div>
    ))
}
```

## 关键文件索引

### 后端启动
- `src-tauri/src/main.rs:4` - Rust 入口
- `src-tauri/src/lib.rs:19` - Tauri 应用配置

### 前端启动
- `index.html:10` - HTML 入口
- `src/main.tsx:16` - 日志初始化
- `src/main.tsx:19` - React 主组件
- `src/apis/LogApi.ts:4` - 日志系统实现

### 应用初始化
- `src/App.tsx:83` - App 组件加载
- `src/core/IoCRegistration.ts:22` - ViewModel 注册
- `src/core/AppInitializer.ts:53` - 核心初始化器

### 数据库层
- `src/storage/index.ts:60` - Storage API 单例
- `src/storage/db/DatabaseInitializer.ts:11` - 数据库初始化
- `src/storage/db/Client.ts` - 数据库客户端
- `src/storage/db/Schema.ts` - 数据库 Schema

### 数据迁移
- `src/storage/migration/MigrationBase.ts` - 迁移基类
- `src/storage/migration/ConfigMigrationV2.ts` - V2 配置迁移
- `src/storage/migration/ConfigMigrationV3.ts` - V3 配置迁移
- `src/storage/migration/ConfigMigrationV4.ts` - V4 配置迁移

### ViewModel 层
- `src/AppViewModel.ts:138` - AppViewModel 单例
- `src/AppViewModel.ts:107` - 初始化流程
- `src/core/BaseViewModel.ts` - ViewModel 基类

## 启动流程图

```
┌─────────────────────────────────────────────────────────────────┐
│ 1. Rust 后端启动                                                 │
│    src-tauri/src/main.rs:4                                      │
│    └─> mintcat_lib::run()                                       │
│        ├─> 初始化 Sentry 错误追踪                                │
│        ├─> 配置 Tauri 插件 (store, clipboard, updater, etc.)    │
│        ├─> 注册命令处理器                                        │
│        └─> 启动 Tauri 应用                                       │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 2. 前端资源加载                                                  │
│    index.html:10                                                │
│    └─> <script src="/src/main.tsx">                             │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 3. 日志系统初始化                                                │
│    src/main.tsx:16                                              │
│    └─> InitLog()                                                │
│        ├─> 拦截 console 方法                                     │
│        ├─> 转发到 Tauri 日志插件                                 │
│        └─> 设置全局错误处理器                                    │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 4. React 应用启动                                                │
│    src/main.tsx:19                                              │
│    └─> ReactDOM.render(<Main/>)                                 │
│        ├─> I18nextProvider (国际化)                             │
│        ├─> ConfigProvider (Ant Design 主题)                     │
│        ├─> HashRouter (路由)                                    │
│        └─> useEventListener("theme-change")                     │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 5. 依赖注入注册                                                  │
│    src/App.tsx:87                                               │
│    └─> registerViewModels()                                     │
│        ├─> IoC.register(AppViewModel)                           │
│        ├─> IoC.register(TreeViewModel)                          │
│        ├─> IoC.register(HomeViewModel)                          │
│        └─> IoC.register(ProfileViewModel)                       │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 6. 核心初始化                                                    │
│    src/App.tsx:90                                               │
│    └─> AppInitializer.initializeCore()                          │
│                                                                  │
│    ┌──────────────────────────────────────────────────────┐    │
│    │ Phase 1: Database (InitPhase.Database)              │    │
│    │ src/core/AppInitializer.ts:66                        │    │
│    │ └─> StorageAPI.getInstance()                         │    │
│    │     └─> DatabaseInitializer.initializeDatabase()     │    │
│    │         ├─> checkTableExists("games")                │    │
│    │         ├─> enableForeignKeys()                      │    │
│    │         ├─> createTables() (执行 SQL DDL)            │    │
│    │         └─> setDefaultValues() (插入默认游戏/用户)   │    │
│    └──────────────────────────────────────────────────────┘    │
│                            ↓                                     │
│    ┌──────────────────────────────────────────────────────┐    │
│    │ Phase 2: Migration (InitPhase.Migration)            │    │
│    │ src/core/AppInitializer.ts:71                        │    │
│    │ └─> MigrationBase.autoMigrate()                      │    │
│    │     ├─> hasExistingData() (检查是否需要迁移)         │    │
│    │     ├─> getExistingConfigList() (V2/V3/V4)          │    │
│    │     ├─> migrateToDatabase() (迁移配置数据)          │    │
│    │     └─> ensureDefaultProfile() (创建默认 profile)   │    │
│    └──────────────────────────────────────────────────────┘    │
│                            ↓                                     │
│    ┌──────────────────────────────────────────────────────┐    │
│    │ Phase 3: CoreViewModel (InitPhase.CoreViewModel)    │    │
│    │ src/core/AppInitializer.ts:76                        │    │
│    │ └─> IoC.get(AppViewModel)                            │    │
│    │     └─> AppViewModel.getInstance()                   │    │
│    │         └─> initialize()                             │    │
│    │             ├─> loadUserLanguages()                  │    │
│    │             ├─> loadUserGuiTheme()                   │    │
│    │             ├─> loadUserInfo()                       │    │
│    │             ├─> loadGameInfo()                       │    │
│    │             ├─> checkAppPath()                       │    │
│    │             ├─> checkOauth()                         │    │
│    │             └─> IntegrateApi.checkGamePath()         │    │
│    └──────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────────┐
│ 7. UI 渲染                                                       │
│    src/App.tsx:93                                               │
│    └─> setIsAppViewModelReady(true)                             │
│        ├─> 加载 HomePage 组件                                    │
│        ├─> 初始化剪贴板监听器                                    │
│        └─> 显示应用主界面                                        │
└─────────────────────────────────────────────────────────────────┘
```

## 错误处理

### 初始化失败处理

**AppInitializer** (src/core/AppInitializer.ts:81-86)

```typescript
catch (error) {
    this.currentPhase = InitPhase.Failed;
    this.error = error instanceof Error ? error : new Error(String(error));
    console.error('[AppInitializer] Initialization failed:', error);
    throw this.error;
}
```

**App 组件** (src/App.tsx:96-100)

```typescript
.catch((error) => {
    console.error('[App] Core initialization failed:', error);
    emitEvent('app-error', error.message || 'Application initialization failed');
});
```

### 全局错误捕获

**LogApi** (src/apis/LogApi.ts:44-78)

- 未处理的 Promise rejection (`unhandledrejection` 事件)
- 同步错误 (`error` 事件)
- window.onerror 重写

所有错误都会:
1. 打印到控制台
2. 通过 Tauri 日志插件持久化
3. (生产环境) 上报到 Sentry

## 性能优化要点

### 1. 懒加载策略

- **ViewModel 注册**: 注册到 IoC 容器但不立即初始化
- **页面组件**: 点击菜单时才加载对应页面组件 (src/App.tsx:42-74)

### 2. 单例模式

- **StorageAPI**: 全局唯一实例，避免重复初始化数据库
- **AppViewModel**: 全局唯一实例，共享应用状态
- **数据库连接**: 单例数据库客户端

### 3. 异步初始化

- 所有初始化操作都是异步的，不阻塞 UI 渲染
- 使用 Promise 链式调用保证顺序

### 4. 条件初始化

- 数据库表已存在时跳过创建
- 已有数据时跳过迁移
- 已初始化时直接返回实例

## 常见问题

### Q: 为什么数据库初始化在 AppViewModel 之前?

A: AppViewModel 需要读取数据库中的用户设置 (语言、主题、用户信息等)，因此必须先初始化数据库。

### Q: 数据迁移什么时候触发?

A: 当数据库表已创建但 mods 表为空时，系统会检查旧版本配置文件 (V2/V3/V4) 并迁移数据。

### Q: 如何确保初始化只执行一次?

A: 使用静态变量和锁机制:
- `AppInitializer.currentPhase` 记录初始化阶段
- `StorageAPI.initPromise` 防止并发初始化
- `AppViewModel.lockInstance` 提供线程安全的单例

### Q: 首次启动和正常启动有什么区别?

A: 首次启动时 (`DeviceApi.isFirstRun()`):
1. 数据库表不存在，会创建表结构
2. 无旧配置，不触发迁移
3. 自动打开配置管理对话框

### Q: 启动失败如何调试?

A: 查看日志文件 (Tauri 日志插件输出):
1. 检查 `[AppInitializer]` 日志确定失败阶段
2. 检查 `[Global Error]` 日志查看异常信息
3. 生产环境可查看 Sentry 错误报告

## 相关文档

- [架构分层设计](./architecture-layers.md)
- [当前构建与启动入口](../../README.md)
- [模块化运行时架构](./modular-runtime-architecture.md)
- [执行验收记录](./modular-refactor-todo.md)
