# Mod 安装顺序机制设计文档

> 历史文档：保留旧规则的背景，不再以旧 DAO/ViewModel/ModInstallTask 调用链作为现状。当前安装输入来自固定 Profile snapshot、内容 revision 与模块闭包；引擎按固定输入顺序产生 winner/conflict manifest，批准绑定不可变 DeployPlan，文件 journal 提交生成 receipt。当前架构与验收见 [模块化运行时架构](modular-runtime-architecture.md) 和 [执行记录](modular-refactor-todo.md)。

## 概述

本文档详细说明 MintCat 应用中 Mod 安装顺序的确定机制、数据流程和相关组件的技术实现。

## 1. 核心机制

Mod 安装顺序由数据库中 `profile_mods` 表的 **`sortOrder`** 字段决定，该字段在整个系统中保持一致性，从数据库读取到最终安装执行都严格按照此顺序进行。

### 1.1 关键特性

- ✅ **持久化排序**：sortOrder 存储在数据库中，重启应用后保持不变
- ✅ **自动排序**：从数据库读取时自动按 sortOrder 升序排列
- ✅ **UI 同步**：界面显示顺序与安装顺序完全一致
- ✅ **支持重排**：通过排序按钮和拖拽操作可以修改 sortOrder

## 2. 数据模型

### 2.1 数据库表结构

#### `profile_mods` 表

| 字段 | 类型 | 说明 |
|------|------|------|
| `profileId` | INTEGER | 配置 ID（外键） |
| `modId` | INTEGER | Mod ID（外键） |
| `parentFolderId` | INTEGER | 父文件夹 ID |
| `sortOrder` | INTEGER | **排序顺序（核心字段）** |
| `isEnabled` | INTEGER | 是否启用（0/1） |
| `usedVersion` | TEXT | 使用的版本 |
| `createdAt` | INTEGER | 创建时间戳 |
| `updatedAt` | INTEGER | 更新时间戳 |

**索引**：
```sql
CREATE INDEX profile_mods_profile_sort_idx
ON profile_mods(profileId, sortOrder);
```

#### `profile_folders` 表

| 字段 | 类型 | 说明 |
|------|------|------|
| `folderId` | INTEGER | 文件夹 ID（主键） |
| `profileId` | INTEGER | 配置 ID（外键） |
| `name` | TEXT | 文件夹名称 |
| `parentFolderId` | INTEGER | 父文件夹 ID |
| `folderType` | TEXT | 文件夹类型（default/custom） |
| `sortOrder` | INTEGER | **排序顺序** |
| `createdAt` | INTEGER | 创建时间戳 |
| `updatedAt` | INTEGER | 更新时间戳 |

### 2.2 sortOrder 分配规则

| 场景 | sortOrder 值 | 说明 |
|------|--------------|------|
| 新增 Mod | `maxSortOrder + 1` | 追加到列表末尾 |
| 排序操作 | `0, 1, 2, 3, ...` | 重新分配连续序号 |
| 拖拽操作 | `0, 1, 2, 3, ...` | 根据新位置重新分配 |
| 文件夹内 Mod | 独立的 `0, 1, 2, ...` | 每个文件夹内独立排序 |

## 3. 核心组件

### 3.1 数据访问层

| 组件 | 文件路径 | 职责 |
|------|----------|------|
| ProfileDAO | `src/storage/dao/ProfileDAO.ts` | 数据库操作，自动按 sortOrder 排序 |
| Schema | `src/storage/db/Schema.ts` | 数据库表定义 |

#### 关键方法：`getProfileMods()`

**位置**：`src/storage/dao/ProfileDAO.ts:405-416`

```typescript
public async getProfileMods(profileId: number): Promise<ProfileModData[]> {
    const db = await getDb();
    const result = await db.select().from(profileMods)
        .where(eq(profileMods.profileId, profileId))
        .orderBy(profileMods.sortOrder); // ← 自动按 sortOrder 升序排列
    return result.map(this.mapToProfileModData);
}
```

**特点**：
- 所有读取操作都自动按 sortOrder 排序
- 保证数据一致性
- 无需在业务层手动排序

### 3.2 业务逻辑层

| 组件 | 文件路径 | 职责 |
|------|----------|------|
| ProfileTreeService | `src/services/ProfileTreeService.ts` | 树结构管理、排序、保存 |
| ModService | `src/services/ModService.ts` | Mod 添加、删除等操作 |

#### 新增 Mod 时的 sortOrder 分配

**位置**：`src/pages/HomePage/HomeViewModel.ts:90-98`

```typescript
const profileMods = await profiles.getProfileMods(activeProfile.id!);
const maxSortOrder = profileMods.reduce(
    (max, pm) => Math.max(max, pm.sortOrder ?? 0),
    -1
);

await profiles.addModToProfile({
    profileId: activeProfile.id!,
    modId: existingMod.modId!,
    parentFolderId: groupId,
    sortOrder: maxSortOrder + 1, // ← 追加到末尾
    isEnabled: true,
    usedVersion: modVersion?.currentVersion || "",
});
```

#### 保存树结构时更新 sortOrder

**位置**：`src/services/ProfileTreeService.ts:464-503`

```typescript
private async saveProfileTreeItems(
    items: ProfileTreeItem[],
    profileDAO: any,
    profileId: number,
    parentFolderId: number | null,
    sortOrder: number
): Promise<void> {
    for (let i = 0; i < items.length; i++) {
        const item = items[i];

        if (item.type === ProfileTreeType.FOLDER) {
            const folder = await profileDAO.createFolder({
                profileId,
                name: item.name,
                parentFolderId,
                folderType: 'custom',
                sortOrder: sortOrder + i  // ← 根据位置分配 sortOrder
            });
            // 递归保存子节点
            await this.saveProfileTreeItems(
                item.children,
                profileDAO,
                profileId,
                folder.folderId,
                0  // 子节点从 0 开始
            );
        } else if (item.type === ProfileTreeType.ITEM) {
            await profileDAO.addModToProfile({
                profileId,
                modId: item.id,
                parentFolderId,
                sortOrder: sortOrder + i,  // ← 根据位置分配 sortOrder
                isEnabled: item.enabled,
                usedVersion: item.usedVersion
            });
        }
    }
}
```

### 3.3 UI 层

| 组件 | 文件路径 | 职责 |
|------|----------|------|
| HomePage | `src/pages/HomePage/index.tsx` | 主页面，包含排序按钮 |
| TreeView | `src/pages/HomePage/TreeView.tsx` | 树形视图，支持拖拽 |
| TreeViewModel | `src/pages/HomePage/TreeViewModel.ts` | 树视图的 ViewModel |

### 3.4 任务执行层

| 组件 | 文件路径 | 职责 |
|------|----------|------|
| ModInstallTask | `src/tasks/ModInstallTask.ts` | Mod 安装任务 |
| IntegrateApi | `src/apis/IntegrateApi.ts` | 游戏集成 API |

### 3.5 Rust 后端

| 组件 | 文件路径 | 职责 |
|------|----------|------|
| pak_integrator | `src-tauri/src/integrator/drg/pak_integrator.rs` | DRG pak 文件集成 |
| ue4ss_integrate | `src-tauri/src/integrator/ue4ss/ue4ss_integrate.rs` | UE4SS mod 安装 |

## 4. 完整数据流程

### 4.1 Mod 安装流程

```
┌─────────────────────────────────────────────────────────────┐
│ 1. 用户触发安装                                              │
│    - 点击"启动游戏"按钮                                      │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 2. ModInstallTask 读取 Profile Mods                         │
│    src/tasks/ModInstallTask.ts:62-83                        │
│                                                              │
│    const profileMods = await profilesDAO.getProfileMods()   │
│    // ← 自动按 sortOrder 排序                               │
│                                                              │
│    const enabledMods = profileMods.filter(pm => pm.isEnabled)│
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 3. 构建安装列表（保持 sortOrder 顺序）                      │
│    src/tasks/ModInstallTask.ts:184-195                      │
│                                                              │
│    for (const item of enabledMods) {                        │
│        installModList.push({                                │
│            name: modName,                                   │
│            modio_id: item.platformId,                       │
│            pak_path: item.download?.cachePath               │
│        });                                                  │
│    }                                                        │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 4. 调用 Rust 后端安装                                        │
│    src/apis/IntegrateApi.ts                                 │
│                                                              │
│    await IntegrateApi.install(                              │
│        drgPakPath,                                          │
│        JSON.stringify(installModList)                       │
│    );                                                       │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 5. Rust 后端按顺序处理                                       │
│    src-tauri/src/integrator/drg/pak_integrator.rs:135-179  │
│                                                              │
│    for (current_index, mod_info) in mods.iter_mut()        │
│                                                .enumerate() {│
│        self.process_mod(mod_info);                          │
│    }                                                        │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 资源冲突与覆盖规则

当多个 Mod 包含**相同资源路径**（如同一 uasset/uexp 文件）时，后端通过 `added_paths` 集合决定写入顺序：

- **规则**：先被处理的 Mod 先写入；若某路径已被写入，后续 Mod 中的同路径文件会**被跳过**，不再写入。
- **顺序**：后端按前端传入的列表顺序依次处理（列表从上到下 = sortOrder 升序）。
- **结论**：**列表上方的 Mod 优先生效**，同路径时下方的 Mod 不会覆盖上方的；即「下方的被上方的覆盖」，而非「上方的被下方的覆盖」。

**实现位置**：
- DRG：`src-tauri/src/integrator/drg/pak_integrator.rs` 中 `write_mod_assets`（约 453-455 行）与 `process_unpacked_mod`（约 295-299 行）在写入前检查 `added_paths`，已存在则 `continue`。
- RC：`src-tauri/src/integrator/drgrc/pak_integrator.rs` 中逻辑一致。

### 4.3 排序操作流程

```
┌─────────────────────────────────────────────────────────────┐
│ 用户操作                                                     │
│  - 点击升序/降序/时间排序按钮                                │
│  - 拖拽 Mod 改变位置                                         │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ HomePage.onMenuBarSortClick() / TreeView.onDrop()           │
│ src/pages/HomePage/index.tsx:367-371                        │
│ src/pages/HomePage/TreeView.tsx:38-73                       │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ TreeViewModel.sortMods()                                    │
│ src/pages/HomePage/TreeViewModel.ts:34-45                   │
│                                                              │
│  1. 获取当前树结构                                           │
│  2. 调用 ProfileTreeService.sortTreeNodes() 排序            │
│  3. 调用 saveProfileTreeToDatabase() 保存                   │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ ProfileTreeService.sortTreeNodes()                          │
│ src/services/ProfileTreeService.ts:340-376                  │
│                                                              │
│  - "asc": 按名称升序排序                                     │
│  - "desc": 按名称降序排序                                    │
│  - "time": 按更新时间排序                                    │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ ProfileTreeService.saveProfileTreeItems()                   │
│ src/services/ProfileTreeService.ts:464-503                  │
│                                                              │
│  遍历所有节点，重新分配 sortOrder:                           │
│  - sortOrder = 0, 1, 2, 3, ...                              │
│  - 更新数据库中的 profile_mods 和 profile_folders 表        │
└─────────────────────────────────────────────────────────────┘
                            ↓
┌─────────────────────────────────────────────────────────────┐
│ 数据库更新完成                                               │
│  - 新的 sortOrder 值已持久化                                │
│  - 下次安装将按新顺序执行                                    │
└─────────────────────────────────────────────────────────────┘
```

## 5. 排序功能详解

### 5.1 UI 排序按钮

**位置**：`src/pages/HomePage/index.tsx:707-724`

```typescript
{/* 升序排序 */}
<Button icon={<SortAscendingOutlined/>}
        type={"text"}
        onClick={() => this.onMenuBarSortClick("asc")}
/>

{/* 降序排序 */}
<Button icon={<SortDescendingOutlined/>}
        type={"text"}
        onClick={() => this.onMenuBarSortClick("desc")}
/>

{/* 按时间排序 */}
<Button icon={<FieldTimeOutlined/>}
        type={"text"}
        onClick={() => this.onMenuBarSortClick("time")}
/>
```

### 5.2 排序算法

**位置**：`src/services/ProfileTreeService.ts:340-376`

```typescript
private sortNodeRecursive(node: ProfileTreeItem, order: string): void {
    if (!node.children || node.children.length === 0) {
        return;
    }

    // 递归排序子节点
    node.children.forEach(child => this.sortNodeRecursive(child, order));

    // 排序当前层级
    node.children.sort((a, b) => {
        if (order === 'asc') {
            // 升序：A → Z
            return a.displayName.localeCompare(b.displayName);
        } else if (order === 'desc') {
            // 降序：Z → A
            return b.displayName.localeCompare(a.displayName);
        } else if (order === 'time') {
            // 按时间：最新更新优先
            const timeA = a.updatedAt || 0;
            const timeB = b.updatedAt || 0;
            return timeB - timeA;
        }
        return 0;
    });
}
```

### 5.3 拖拽排序

**位置**：`src/pages/HomePage/TreeView.tsx:38-73`

```typescript
@autoBind
private async onDrop(info: any) {
    const treeData = this.props.treeData;

    // 执行拖拽逻辑，更新树结构
    const dragTreeData = dragAndDrop(info, treeData);

    try {
        // 转换为 ProfileTreeItem 格式
        const converter = new TreeViewConverter();
        const profileTreeItem = converter.convertFrom(dragTreeData);

        if (!profileTreeItem || profileTreeItem.children.length === 0) {
            return;
        }

        // 保存到数据库（会重新分配 sortOrder）
        const profileVM = await IoC.get(ProfileViewModel);
        await profileVM.saveProfileTreeToDatabase(profileTreeItem);

        // 刷新 UI
        if (this.props.onUpdateTreeView) {
            await this.props.onUpdateTreeView();
        }
    } catch (error) {
        console.error(`[TreeView] Error during drag and drop:`, error);
    }
}
```

**拖拽逻辑**：`src/pages/HomePage/DragAndDropTree.ts`
- 处理节点移动
- 更新父子关系
- 重新排列兄弟节点顺序

## 6. 特殊场景处理

### 6.1 文件夹内的 Mod 排序

每个文件夹内的 Mod 有独立的 sortOrder 序列：

```
Root
├── Folder A (sortOrder: 0)
│   ├── Mod 1 (sortOrder: 0, parentFolderId: A)
│   ├── Mod 2 (sortOrder: 1, parentFolderId: A)
│   └── Mod 3 (sortOrder: 2, parentFolderId: A)
├── Folder B (sortOrder: 1)
│   ├── Mod 4 (sortOrder: 0, parentFolderId: B)
│   └── Mod 5 (sortOrder: 1, parentFolderId: B)
└── Mod 6 (sortOrder: 2, parentFolderId: null)
```

### 6.2 默认文件夹处理

**位置**：`src/services/ProfileTreeService.ts:509-551`

默认文件夹（如 "Approved Mods"）的 Mod 也遵循 sortOrder 规则：

```typescript
private async saveDefaultFolderItems(
    item: ProfileTreeItem,
    profileDAO: any,
    profileId: number,
    defaultFolderId: number
): Promise<void> {
    const mods = item.children.filter(child =>
        child.type === ProfileTreeType.ITEM
    );

    for (let i = 0; i < mods.length; i++) {
        await profileDAO.addModToProfile({
            profileId,
            modId: mods[i].id,
            parentFolderId: defaultFolderId,
            sortOrder: i,  // ← 从 0 开始分配
            isEnabled: mods[i].enabled,
            usedVersion: mods[i].usedVersion
        });
    }
}
```

### 6.3 禁用的 Mod

禁用的 Mod 仍然保留 sortOrder 值，但在安装时被过滤：

**位置**：`src/tasks/ModInstallTask.ts:62-83`

```typescript
const profileMods = await profilesDAO.getProfileMods(activeProfileData.id!);
const enabledProfileMods = profileMods.filter(pm => pm.isEnabled); // ← 过滤禁用的 Mod

// 只安装启用的 Mod，但保持相对顺序
for (const pm of enabledProfileMods) {
    const modData = await modsDAO.getCompleteModData(pm.modId);
    if (modData) {
        enabledMods.push(modData);
    }
}
```

## 7. 关键代码位置索引

### 7.1 数据库层

| 功能 | 文件 | 行号 |
|------|------|------|
| profile_mods 表定义 | `src/storage/db/Schema.ts` | 192-206 |
| profile_folders 表定义 | `src/storage/db/Schema.ts` | 170-186 |
| sortOrder 索引 | `src/storage/db/Schema.ts` | 204 |
| getProfileMods() | `src/storage/dao/ProfileDAO.ts` | 405-416 |
| getProfileFolders() | `src/storage/dao/ProfileDAO.ts` | 302-313 |
| buildProfileTree() | `src/storage/dao/ProfileDAO.ts` | 553-617 |

### 7.2 业务逻辑层

| 功能 | 文件 | 行号 |
|------|------|------|
| 新增 Mod 分配 sortOrder | `src/pages/HomePage/HomeViewModel.ts` | 90-98 |
| 新增 Mod (Service) | `src/services/ModService.ts` | 26-36 |
| 排序算法 | `src/services/ProfileTreeService.ts` | 340-376 |
| 保存树结构 | `src/services/ProfileTreeService.ts` | 67-112 |
| 保存树节点 | `src/services/ProfileTreeService.ts` | 464-503 |
| 保存默认文件夹 | `src/services/ProfileTreeService.ts` | 509-551 |

### 7.3 UI 层

| 功能 | 文件 | 行号 |
|------|------|------|
| 排序按钮 | `src/pages/HomePage/index.tsx` | 707-724 |
| 排序按钮点击 | `src/pages/HomePage/index.tsx` | 367-371 |
| 拖拽处理 | `src/pages/HomePage/TreeView.tsx` | 38-73 |
| TreeViewModel.sortMods() | `src/pages/HomePage/TreeViewModel.ts` | 34-45 |
| 拖拽逻辑 | `src/pages/HomePage/DragAndDropTree.ts` | - |

### 7.4 任务执行层

| 功能 | 文件 | 行号 |
|------|------|------|
| 读取 Profile Mods | `src/tasks/ModInstallTask.ts` | 62-83 |
| 构建安装列表 | `src/tasks/ModInstallTask.ts` | 184-195 |
| 调用 Rust 安装 | `src/tasks/ModInstallTask.ts` | 197 |

### 7.5 Rust 后端

| 功能 | 文件 | 行号 |
|------|------|------|
| DRG pak 安装 | `src-tauri/src/integrator/drg/pak_integrator.rs` | 135-179 |
| UE4SS mod 安装 | `src-tauri/src/integrator/ue4ss/ue4ss_integrate.rs` | 112-139 |

## 8. 总结

### 8.1 核心要点

1. **单一数据源**：sortOrder 字段是唯一的排序依据
2. **自动排序**：数据库查询自动按 sortOrder 排序
3. **持久化**：所有排序操作都会更新数据库
4. **UI 一致性**：界面显示顺序 = 安装顺序
5. **支持重排**：排序按钮和拖拽都会更新 sortOrder

### 8.2 操作与 sortOrder 的关系

| 操作 | 是否更新 sortOrder | 更新方式 |
|------|-------------------|----------|
| 添加新 Mod | ✅ 是 | `maxSortOrder + 1` |
| 点击排序按钮 | ✅ 是 | 重新分配 `0, 1, 2, 3, ...` |
| 拖拽 Mod | ✅ 是 | 根据新位置重新分配 |
| 启用/禁用 Mod | ❌ 否 | 保持原 sortOrder |
| 删除 Mod | ❌ 否 | 其他 Mod 的 sortOrder 不变 |
| 安装 Mod | ❌ 否 | 只读取，不修改 |

### 8.3 设计优势

- ✅ **简单明确**：单一字段控制顺序，易于理解和维护
- ✅ **性能优化**：数据库索引支持快速排序查询
- ✅ **用户友好**：所见即所得，UI 顺序即安装顺序
- ✅ **灵活可控**：支持多种排序方式和手动调整

### 8.4 注意事项

1. **删除 Mod 后的间隙**：删除 Mod 不会重新分配 sortOrder，可能出现 `0, 1, 3, 5` 这样的非连续序列，但不影响功能
2. **文件夹独立性**：每个文件夹内的 Mod 有独立的 sortOrder 序列
3. **禁用 Mod 的顺序**：禁用的 Mod 保留 sortOrder，重新启用后顺序不变
4. **并发安全**：sortOrder 分配使用 `maxSortOrder + 1`，避免冲突
5. **同路径覆盖规则**：打包时同一资源路径只保留**先被处理**的那份（即列表上方的 Mod 优先）；若希望某 Mod 的资源生效，应将其排在列表更上方

## 9. 相关文档

- [树节点拖拽系统设计文档](./tree-drag-and-drop.md)
- [应用启动流程](./application-startup-flow.md)
- [架构分层设计](./architecture-layers.md)
