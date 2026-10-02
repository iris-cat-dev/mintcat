# Mod 更新与下载流程设计文档

> 历史文档：旧 TypeScript provider/renderer 任务编排不再运行，根目录 `src/`、`src-tauri/` 和旧 `TimeUtils` 已清理；文中的旧源码路径仅是历史索引。当前来源 worker、Network/Credential broker、共享 transfer、artifact grants 和 Library 条件提交由独立 daemon 协调；下载与列表编辑的原子边界见 [下载并发设计](mod-download-concurrency.md)，实现与验收见 [执行记录](modular-refactor-todo.md)。

## 概述

本文档详细描述 MintCat 应用中 Mod 的更新检测、下载和安装的完整流程，包括各服务层的职责、数据流向和事件机制。

## 1. 架构概览

### 1.1 核心组件

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              UI Layer                                    │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐       │
│  │  TreeViewItem    │  │   HomePage       │  │   StatusBar      │       │
│  │  (显示/交互)      │  │   (列表管理)      │  │   (状态提示)      │       │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘       │
└───────────┼────────────────────┼────────────────────┼───────────────────┘
            │                    │                    │
            ▼                    ▼                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                           Service Layer                                  │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐       │
│  │ ModUpdateService │  │   HomeViewModel  │  │ ProfileViewModel │       │
│  │ (更新/下载逻辑)    │  │   (业务逻辑)      │  │   (配置管理)      │       │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘       │
└───────────┼────────────────────┼────────────────────┼───────────────────┘
            │                    │                    │
            ▼                    ▼                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                            API Layer                                     │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐       │
│  │    ModioApi      │  │   DownloadApi    │  │    CacheApi      │       │
│  │  (mod.io 接口)    │  │   (下载管理)      │  │   (缓存管理)      │       │
│  └────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘       │
└───────────┼────────────────────┼────────────────────┼───────────────────┘
            │                    │                    │
            ▼                    ▼                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                           Data Layer                                     │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐       │
│  │    StorageAPI    │  │     ModDAO       │  │   ProfileDAO     │       │
│  │   (统一入口)      │  │   (Mod 数据)      │  │   (配置数据)      │       │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘       │
└─────────────────────────────────────────────────────────────────────────┘
```

### 1.2 任务系统

```
┌─────────────────────────────────────────────────────────────────────────┐
│                          Task Queue System                               │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐       │
│  │CheckModUpdateTask│  │  ModInstallTask  │  │  CheckOAuthTask  │       │
│  │  (检查更新)        │  │  (安装模组)       │  │  (验证授权)       │       │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘       │
└─────────────────────────────────────────────────────────────────────────┘
```

## 2. 核心服务说明

### 2.1 ModUpdateService

**位置**: `src/services/ModUpdateService.ts`

**职责**:
- 检查模组更新
- 下载模组文件
- 检查本地模组缓存
- 检查本地模组修改

**主要方法**:

| 方法 | 说明 |
|------|------|
| `updateMod(mod)` | 更新单个模组（获取在线信息 + 下载） |
| `batchUpdateMods(mods)` | 批量更新模组在线信息（不下载） |
| `updateModFile(mod)` | 下载模组文件 |
| `updateModInDatabase(modId, modInfo)` | 更新数据库中的模组元信息 |
| `checkOnlineModAndUpdate(mod, isEnabled)` | 检查在线模组是否需要更新 |
| `checkLocalModCache(mod)` | 检查本地模组缓存文件是否存在 |
| `checkLocalModModify(mod, isEnabled)` | 检查本地模组是否被修改 |
| `checkModList()` | 检查所有模组的本地缓存状态 |
| `checkModUpdate()` | 通过任务系统检查在线更新 |

### 2.2 ModioApi

**位置**: `src/apis/modio/index.ts`

**职责**:
- 与 mod.io API 交互
- 获取模组信息
- 下载模组文件

**主要方法**:

| 方法 | 说明 |
|------|------|
| `getModInfoByLink(url)` | 通过 URL 获取模组信息 |
| `getModInfoByIdList(modIds)` | 批量获取模组信息 |
| `getEvents(dateAdded, modIds)` | 获取模组更新事件 |
| `getModFiles(modId)` | 获取模组的所有版本文件 |
| `downloadModFile(modInfo, onProgress)` | 下载模组文件 |

### 2.3 DownloadApi

**位置**: `src/apis/DownloadApi.ts`

**职责**:
- 统一的文件下载管理
- 支持断点续传
- 支持重试机制
- 进度回调

**主要方法**:

| 方法 | 说明 |
|------|------|
| `downloadFile(url, filePath, options, progressCallback)` | 下载文件到指定路径 |
| `cancelDownload(downloadId)` | 取消下载 |

**下载选项**:
```typescript
type DownloadOptions = {
    checksum?: string,      // 校验和
    checksumType?: 'md5' | 'sha256',
    timeoutSecs?: number,   // 超时时间
    retryCount?: number,    // 重试次数
    resume?: boolean        // 断点续传
}
```

### 2.4 CacheApi

**位置**: `src/apis/CacheApi.ts`

**职责**:
- 缓存路径管理
- 缓存文件校验
- 缓存清理

**主要方法**:

| 方法 | 说明 |
|------|------|
| `getCacheDir()` | 获取缓存目录 |
| `getModCachePath(modName, version)` | 获取模组缓存文件路径 |
| `checkCacheFile(modName, version, fileSize)` | 校验缓存文件是否完整 |
| `saveCacheFile(modName, version, data)` | 保存缓存文件 |

**缓存文件命名规则**:
```
{modName}-{version}.zip
```

## 3. 更新检测流程

### 3.1 主动更新检测（CheckModUpdateTask）

通过 mod.io Events API 检测是否有模组更新。

```
┌──────────────────────────────────────────────────────────────────────────┐
│                    CheckModUpdateTask 执行流程                            │
└──────────────────────────────────────────────────────────────────────────┘

     调用入口
         │
         ▼
┌─────────────────┐
│ checkModUpdate()│  ModUpdateService 入口
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ 添加任务到队列    │  taskQueueAPI.addTask('check_mod_update')
└────────┬────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Step 1: 加载模组列表                            │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ 1. 获取上次检查时间 (lastUpdate)                          │    │
│  │ 2. 如果没有，默认为 30 天前                                │    │
│  │ 3. 获取所有 Modio 类型的 mod，提取 platformId 列表         │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Step 2: 获取更新信息                            │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ ModioApi.getEvents(lastUpdate, modIdList)               │    │
│  │                                                          │    │
│  │ API 请求:                                                │    │
│  │ GET /games/{gameId}/mods/events                         │    │
│  │   ?mod_id-in={modIds}                                   │    │
│  │   &date_added-min={lastUpdate}                          │    │
│  │   &event_type-in=MODFILE_CHANGED,MOD_UNAVAILABLE,       │    │
│  │                  MOD_DELETED                             │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Step 3: 处理更新信息                            │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ for each event:                                          │    │
│  │                                                          │    │
│  │   MODFILE_CHANGED:                                       │    │
│  │     └─ upsertModStatus({                                │    │
│  │          modId,                                          │    │
│  │          onlineUpdateDate: event.date_added * 1000,     │    │
│  │          lastUpdateDate: 0  // 标记需要更新               │    │
│  │        })                                                │    │
│  │                                                          │    │
│  │   MOD_UNAVAILABLE / MOD_DELETED:                        │    │
│  │     └─ upsertModStatus({                                │    │
│  │          modId,                                          │    │
│  │          isOnlineAvailable: false                       │    │
│  │        })                                                │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────┐
│ 更新检查时间戳    │  profileVM.setActiveProfileLastUpdate()
└─────────────────┘
```

### 3.2 被动更新检测

在启用模组或安装模组时触发检测。

```
┌──────────────────────────────────────────────────────────────────────────┐
│               checkOnlineModAndUpdate() 执行流程                          │
└──────────────────────────────────────────────────────────────────────────┘

     输入: modItem, isEnabled
         │
         ▼
    ┌────────────────┐
    │ sourceType ==  │───No───▶ 返回（跳过本地类型）
    │   Modio?       │
    └───────┬────────┘
            │ Yes
            ▼
    ┌────────────────┐
    │  isEnabled?    │───No───▶ 返回（未启用）
    └───────┬────────┘
            │ Yes
            ▼
    ┌────────────────────────────────────────────────┐
    │             检查是否需要更新                      │
    │                                                 │
    │  需要更新的条件（任一满足）:                       │
    │  1. cachePath 文件不存在                         │
    │  2. onlineUpdateDate > lastUpdateDate          │
    │  3. downloadProgress != 100                    │
    └───────┬─────────────────────────────────────────┘
            │ 需要更新
            ▼
    ┌────────────────┐
    │  updateMod()   │  执行完整更新流程
    └────────────────┘
```

### 3.3 "发现新版本"指示器显示逻辑

UI 组件 `ModTreeViewWarning` 中的 `checkExpired()` 函数决定是否显示橙色时钟图标。

```
┌──────────────────────────────────────────────────────────────────────────┐
│                      checkExpired() 判断流程                              │
└──────────────────────────────────────────────────────────────────────────┘

    开始检查
         │
         ▼
    ┌────────────────┐
    │ sourceType ==  │───No───▶ return false (本地类型不检测)
    │   Modio?       │
    └───────┬────────┘
            │ Yes
            ▼
    ┌────────────────┐
    │downloadProgress│───No───▶ return false (下载未完成)
    │   == 100?      │
    └───────┬────────┘
            │ Yes
            ▼
    ┌────────────────┐
    │ usedVersion    │───Yes──▶ return false (用户手动选择了版本)
    │   不为空?       │
    └───────┬────────┘
            │ No (使用默认最新版)
            ▼
    ┌────────────────────────────────────────┐
    │  TimeUtils.hasUpdate(                  │
    │    onlineUpdateDate,                   │
    │    lastUpdateDate                      │
    │  )                                     │
    │                                        │
    │  返回: lastUpdateDate > 0 &&           │
    │        onlineUpdateDate > lastUpdateDate│
    └────────────────────────────────────────┘
              │
              ▼
    ┌─────────────────────────────────────┐
    │  true  → 显示橙色时钟图标 🕐          │
    │  false → 不显示                      │
    └─────────────────────────────────────┘
```

## 4. 下载流程

### 4.1 单个模组更新下载

```
┌──────────────────────────────────────────────────────────────────────────┐
│                        updateMod() 执行流程                               │
└──────────────────────────────────────────────────────────────────────────┘

     输入: CompleteModData
         │
         ▼
┌─────────────────┐
│ 显示状态:        │  StatusBar.info("更新模组 [modName]")
│ "更新模组..."    │
└────────┬────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 1: 获取在线信息                                 │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ ModioApi.getModInfoByLink(mod.url)                      │    │
│  │                                                          │    │
│  │ 如果获取失败:                                             │    │
│  │   └─ 标记为不可用 (isOnlineAvailable = false)            │    │
│  │   └─ 发送事件更新 UI                                     │    │
│  │   └─ 返回                                                │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 2: 更新数据库信息                               │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ updateModInDatabase(modId, modInfo)                     │    │
│  │                                                          │    │
│  │ 更新内容:                                                │    │
│  │   mods: nameId, url, tags, originalName                 │    │
│  │   mod_versions: currentVersion                          │    │
│  │   mod_downloads: downloadUrl, fileSize                  │    │
│  │   mod_status: onlineUpdateDate, isOnlineAvailable=true  │    │
│  │                                                          │    │
│  │ 发送 mod-treeview-update 事件                            │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 3: 下载文件                                     │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ updateModFile(mod) → 详见 4.2 节                        │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────┐
│ 显示状态:        │  StatusBar.success("更新完成")
│ "更新完成"       │
└─────────────────┘
```

### 4.2 文件下载流程

```
┌──────────────────────────────────────────────────────────────────────────┐
│                      updateModFile() 执行流程                             │
└──────────────────────────────────────────────────────────────────────────┘

     输入: CompleteModData
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 1: 检查缓存                                     │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ ModioApi.downloadModFile() 内部调用:                     │    │
│  │                                                          │    │
│  │ CacheApi.checkCacheFile(nameId, version, fileSize)      │    │
│  │   └─ 检查文件是否存在                                     │    │
│  │   └─ 比较文件大小是否匹配                                 │    │
│  │                                                          │    │
│  │ 如果缓存有效:                                             │    │
│  │   └─ 直接返回 cachePath，跳过下载                         │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │ 缓存无效，需要下载
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 2: 下载文件                                     │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ DownloadApi.downloadFile(                               │    │
│  │   url: downloadUrl,                                     │    │
│  │   filePath: cachePath,                                  │    │
│  │   options: { resume: true, retryCount: 3 },             │    │
│  │   progressCallback                                      │    │
│  │ )                                                        │    │
│  │                                                          │    │
│  │ 进度回调:                                                │    │
│  │   └─ 计算 downloadProgress = (loaded / total) * 100     │    │
│  │   └─ 发送 mod-treeview-update 事件更新 UI               │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 3: 更新数据库                                   │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ upsertModDownload({                                     │    │
│  │   modId,                                                │    │
│  │   cachePath,                                            │    │
│  │   downloadProgress: 100,                                │    │
│  │   downloadStatus: "completed"                           │    │
│  │ })                                                       │    │
│  │                                                          │    │
│  │ upsertModVersion({                                      │    │
│  │   modId,                                                │    │
│  │   currentVersion                                        │    │
│  │ })                                                       │    │
│  │                                                          │    │
│  │ upsertModStatus({                                       │    │
│  │   modId,                                                │    │
│  │   lastUpdateDate: onlineUpdateDate  // 同步时间戳        │    │
│  │ })                                                       │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 4: 发送事件更新 UI                              │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ emitEvent("mod-treeview-update", {                      │    │
│  │   modId,                                                │    │
│  │   data: updatedMod  // 包含最新的 lastUpdateDate        │    │
│  │ })                                                       │    │
│  │                                                          │    │
│  │ 作用: 清除"发现新版本"指示器                              │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

### 4.3 批量更新信息（不下载）

```
┌──────────────────────────────────────────────────────────────────────────┐
│                     batchUpdateMods() 执行流程                            │
└──────────────────────────────────────────────────────────────────────────┘

     输入: CompleteModData[]
         │
         ▼
┌─────────────────┐
│ 筛选 Modio 类型  │  过滤掉 Local 类型的 mod
└────────┬────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 1: 批量获取在线信息                             │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ ModioApi.getModInfoByIdList(platformIds)                │    │
│  │                                                          │    │
│  │ 内部分批处理（每批 20 个）:                                │    │
│  │ GET /games/{gameId}/mods?id-in={batch.join(",")}        │    │
│  └─────────────────────────────────────────────────────────┘    │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│              Step 2: 更新数据库                                   │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ for each mod:                                            │    │
│  │   if (modInfo exists):                                  │    │
│  │     updateModInDatabase(modId, modInfo)                 │    │
│  │       └─ 更新 mods, mod_versions, mod_downloads,        │    │
│  │          mod_status 表                                  │    │
│  │       └─ 发送 mod-treeview-update 事件                  │    │
│  │   else:                                                 │    │
│  │     markModUnavailable(modId)                           │    │
│  │       └─ 设置 isOnlineAvailable = false                 │    │
│  │       └─ 发送事件更新 UI                                 │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘

注意: batchUpdateMods 只更新元信息，不下载文件
      downloadProgress 保持原值，不会被重置
```

## 5. 安装流程

### 5.1 ModInstallTask 执行流程

```
┌──────────────────────────────────────────────────────────────────────────┐
│                      ModInstallTask 执行流程                              │
└──────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│  Step 1: 验证游戏环境                                            │
│    ├─ 检查游戏路径是否存在                                        │
│    └─ 检查游戏是否正在运行（必须关闭）                             │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 2: 加载模组配置                                            │
│    ├─ 获取当前 Profile 的所有 mod                                │
│    └─ 筛选已启用的 mod                                           │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 3: 检查模组更新                                            │
│    for each enabledMod:                                          │
│      └─ checkOnlineModAndUpdate(mod, true)                      │
│           └─ 如果需要更新，自动下载                               │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 4: 验证模组文件                                            │
│    for each enabledMod:                                          │
│      └─ checkLocalModCache(mod)                                 │
│           └─ 检查缓存文件是否存在                                 │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 5: 检查安装状态                                            │
│    └─ IntegrateApi.checkInstalled()                             │
│         ├─ mintcat_installed: 已是最新，直接返回                  │
│         ├─ old_version_mint_installed: 提示卸载旧版               │
│         └─ not_installed: 继续安装                               │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 6: 卸载旧版本                                              │
│    └─ IntegrateApi.uninstall()                                  │
└────────┬────────────────────────────────────────────────────────┘
         │
         ▼
┌─────────────────────────────────────────────────────────────────┐
│  Step 7: 安装模组                                                │
│    └─ IntegrateApi.install(gamePath, modList)                   │
│         └─ 将 mod 文件写入游戏目录                                │
└─────────────────────────────────────────────────────────────────┘
```

## 6. 数据模型

### 6.1 核心数据表关系

```
┌─────────────────┐      ┌─────────────────┐
│      mods       │      │    profiles     │
│─────────────────│      │─────────────────│
│ modId (PK)      │      │ id (PK)         │
│ platformId      │      │ name            │
│ gameId          │      │ isActive        │
│ nameId          │      └────────┬────────┘
│ displayName     │               │
│ url             │               │
│ sourceType      │               │
└───────┬─────────┘               │
        │                         │
        │ 1:1                     │ 1:N
        │                         │
┌───────┴─────────┐      ┌────────┴────────┐
│  mod_versions   │      │  profile_mods   │
│─────────────────│      │─────────────────│
│ modId (PK,FK)   │      │ id (PK)         │
│ currentVersion  │      │ profileId (FK)  │
│ availableVersions│     │ modId (FK)      │
└─────────────────┘      │ isEnabled       │
                         │ usedVersion     │
┌─────────────────┐      └─────────────────┘
│  mod_downloads  │
│─────────────────│
│ modId (PK,FK)   │
│ downloadUrl     │
│ cachePath       │
│ fileSize        │
│ downloadProgress│
│ downloadStatus  │
└─────────────────┘

┌─────────────────┐
│   mod_status    │
│─────────────────│
│ modId (PK,FK)   │
│ lastUpdateDate  │◀─── 本地文件最后更新时间（毫秒）
│ onlineUpdateDate│◀─── 在线最新版本时间（毫秒）
│ isOnlineAvailable│
│ isLocalNotFound │
└─────────────────┘
```

### 6.2 关键时间戳字段说明

| 字段 | 存储单位 | 更新时机 | 用途 |
|------|----------|----------|------|
| `lastUpdateDate` | 毫秒 | 下载完成后设为 `onlineUpdateDate` | 判断是否有新版本 |
| `onlineUpdateDate` | 毫秒 | 从 mod.io API 获取后 ×1000 | 记录在线最新版本时间 |
| `date_updated` (API) | 秒 | mod.io 返回 | 需要 ×1000 转换 |

### 6.3 CompleteModData 结构

```typescript
interface CompleteModData {
    // 基础信息 (mods 表)
    modId: number;
    platformId: number;
    gameId: number;
    nameId: string;
    displayName: string;
    url: string;
    sourceType: ModSourceType;  // 'Modio' | 'Local'
    tags: string[];

    // 版本信息 (mod_versions 表)
    version?: {
        currentVersion: string;
        availableVersions: string[];
    };

    // 下载信息 (mod_downloads 表)
    download?: {
        downloadUrl: string;
        cachePath: string;
        fileSize: number;
        downloadProgress: number;  // 0-100
        downloadStatus: string;
    };

    // 状态信息 (mod_status 表)
    status?: {
        lastUpdateDate: number;      // 毫秒
        onlineUpdateDate: number;    // 毫秒
        isOnlineAvailable: boolean;
        isLocalNotFound: boolean;
    };
}
```

## 7. 事件机制

### 7.1 mod-treeview-update 事件

**用途**: 更新 TreeView 中单个 mod 的显示状态

**触发场景**:
| 场景 | 发送位置 |
|------|----------|
| 更新数据库信息后 | `updateModInDatabase()` |
| 下载进度更新 | `updateModFile()` 进度回调 |
| 下载完成后 | `updateModFile()` |
| 标记为不可用 | `markModUnavailable()` |
| 本地缓存检查后 | `checkLocalModCache()` |
| 本地文件修改检测后 | `checkLocalModModify()` |

**Payload 结构**:
```typescript
{
    modId: number;
    data: CompleteModData;
}
```

### 7.2 download-progress / download-status 事件

**来源**: Tauri 后端下载管理器

**用途**: 通知前端下载进度和状态变化

```typescript
// 进度事件
type DownloadProgress = {
    downloadId: string;
    downloadedBytes: number;
    totalBytes: number;
    speedBytesPerSec: number;
    etaSecs: number;
}

// 状态事件
type DownloadStatus = {
    downloadId: string;
    status: 'completed' | 'failed' | 'cancelled';
    error?: string;
    filePath?: string;
}
```

## 8. 缓存策略

### 8.1 缓存文件路径

```
{CacheDir}/
└── {modName}-{version}.zip
```

**示例**:
```
~/Library/Caches/com.mint.cat/
├── better-post-processing-4.8.1.zip
├── custom-difficulty-2.0.0.zip
└── mission-control-1.5.3.zip
```

### 8.2 缓存校验逻辑

```typescript
async checkCacheFile(modName: string, version: string, fileSize: number): boolean {
    const filePath = await getModCachePath(modName, version);
    const actualSize = await size(filePath);
    return actualSize === fileSize;
}
```

**校验条件**:
1. 文件存在
2. 文件大小与预期一致

## 9. 错误处理

### 9.1 常见错误场景

| 场景 | 处理方式 |
|------|----------|
| mod.io API 401 | 提示授权失效 |
| mod.io API 404 | 标记 mod 为不可用 |
| mod.io API 429 | 提示请求过于频繁 |
| 下载失败 | 自动重试（最多 3 次） |
| 缓存文件损坏 | 重新下载 |
| 游戏路径不存在 | 提示配置游戏路径 |
| 游戏正在运行 | 提示关闭游戏 |

### 9.2 网络错误重试

```typescript
DownloadApi.downloadFile(url, path, {
    resume: true,      // 支持断点续传
    retryCount: 3      // 重试 3 次
});
```

## 10. 相关文件索引

| 文件 | 职责 |
|------|------|
| `src/services/ModUpdateService.ts` | 更新服务核心逻辑 |
| `src/tasks/CheckModUpdateTask.ts` | 检查更新任务 |
| `src/tasks/ModInstallTask.ts` | 安装任务 |
| `src/apis/modio/index.ts` | mod.io API 封装 |
| `src/apis/DownloadApi.ts` | 下载管理 |
| `src/apis/CacheApi.ts` | 缓存管理 |
| `src/pages/HomePage/TreeViewItem.tsx` | UI 组件（显示/交互） |
| `src/storage/dao/ModDAO.ts` | 数据访问层 |
| `src/storage/db/Schema.ts` | 数据库表定义 |
| `src/utils/TimeUtils.ts` | 时间戳工具 |
