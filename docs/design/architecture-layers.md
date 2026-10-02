# MintCat 项目分层架构设计

> 已被模块化架构替代：以下为旧 renderer ViewModel/Service/DAO 分层的历史说明。当前 Workbench 仅装配签名贡献；业务执行通过唯一 Application API、独立 daemon、版本化 worker 契约和实际数据 owner 完成，不存在前端业务 SQL 写者。以 [模块化运行时架构](modular-runtime-architecture.md) 和 [当前执行验收](modular-refactor-todo.md) 为准。

## 架构概览

MintCat 采用经典的分层架构模式，确保职责分离、代码复用和可维护性。

```
┌─────────────────────────────────────┐
│         View 层 (React)              │
│     (只负责 UI 渲染和事件绑定)        │
└──────────────┬──────────────────────┘
               │ 调用
┌──────────────▼──────────────────────┐
│       ViewModel 层                   │
│  (业务逻辑 + UI 交互 + 状态管理)      │
└──────────────┬──────────────────────┘
               │ 调用
┌──────────────▼──────────────────────┐
│        Service 层                    │
│   (数据查询 + 聚合 + 业务服务)        │
└──────────────┬──────────────────────┘
               │ 调用
┌──────────────▼──────────────────────┐
│         DAO 层 (数据访问)            │
│    (直接操作数据库，CRUD 操作)        │
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│       Database (SQLite)              │
└─────────────────────────────────────┘
```

---

## 各层职责详解

### 1. View 层 (React Components)

**位置**: `src/pages/`, `src/components/`, `src/dialogs/`

**职责**:
- ✅ UI 渲染和展示
- ✅ 用户交互事件绑定
- ✅ 调用 ViewModel 方法
- ✅ 展示 ViewModel 返回的数据和状态

**禁止**:
- ❌ 直接访问数据库或 DAO
- ❌ 包含复杂的业务逻辑
- ❌ 直接操作 Service 层

**示例**:
```typescript
// src/pages/HomePage/index.tsx

class HomePage extends BasePage {
    @autoBind
    private async onMenuBarCopyListClick() {
        // ✅ 正确：只调用 ViewModel
        const vm = await IoC.get(HomeViewModel);
        await vm.exportModioUrlsToClipboard();
    }

    @autoBind
    private async onAddModClick() {
        // ❌ 错误：不应该直接调用 Service
        // const service = new ModService();
        // await service.addMod(...);

        // ✅ 正确：通过 ViewModel
        const vm = await IoC.get(HomeViewModel);
        await vm.addModFromUrl(url, folderId);
    }
}
```

---

### 2. ViewModel 层

**位置**: `src/vm/`, `src/pages/*/ViewModel.ts`, `src/dialogs/*/ViewModel.ts`

**职责**:
- ✅ 业务逻辑处理
- ✅ UI 交互逻辑（显示消息、处理错误）
- ✅ 状态管理和数据转换
- ✅ 调用 Service 层获取/处理数据
- ✅ 触发 UI 更新事件

**禁止**:
- ❌ 直接操作数据库（通过 Service 或 DAO）
- ❌ 包含 UI 渲染逻辑

**示例**:
```typescript
// src/pages/HomePage/HomeViewModel.ts

export class HomeViewModel extends BaseViewModel {
    /**
     * 导出当前 profile 的 mod.io URL 列表到剪贴板
     */
    public async exportModioUrlsToClipboard(): Promise<boolean> {
        // 1. 调用 Service 获取数据
        const profileService = new ProfileService();
        const urls = await profileService.getActiveProfileModioUrls();

        // 2. 处理业务逻辑
        if (urls.length === 0) {
            message.warning(t("No mod.io mods found in current profile"));
            return false;
        }

        const list = urls.join("\n") + "\n";

        // 3. UI 交互（复制到剪贴板、显示消息）
        ClipboardApi.setLastClipboardText(list);
        await navigator.clipboard.writeText(list);
        message.success(t("Copied To Clipboard"));

        return true;
    }

    /**
     * 添加 mod 并处理依赖
     */
    public async addModFromUrl(url: string, groupId: number): Promise<boolean> {
        // 1. 获取 mod 信息
        const modInfo = await ModioApi.getModInfoByLink(url);

        // 2. 通过 Service 添加 mod
        const addedMod = await ModService.addModFromModio(modInfo, profileId, groupId);

        // 3. 处理依赖
        if (modInfo.dependencies) {
            await this.addModDependencies(modInfo.id, groupId);
        }

        // 4. 触发 UI 更新
        TreeViewModel.updateTreeView();

        return true;
    }
}
```

---

### 3. Service 层

**位置**: `src/services/`

**职责**:
- ✅ 数据查询和聚合
- ✅ 跨 DAO 的复杂查询
- ✅ 业务规则验证
- ✅ 数据转换和格式化
- ✅ 可复用的业务服务

**禁止**:
- ❌ UI 交互逻辑（消息提示、错误弹窗）
- ❌ 直接操作 React 状态

**示例**:
```typescript
// src/services/ProfileService.ts

export class ProfileService {
    /**
     * 获取活跃 profile 中所有 mod.io 类型的 mod URL 列表
     * 可被多个 ViewModel 复用
     */
    public async getActiveProfileModioUrls(): Promise<string[]> {
        const profilesApi = await StorageAPI.getProfiles();
        const activeProfile = await profilesApi.getActiveProfile();

        if (!activeProfile?.id) {
            return [];
        }

        return await this.getProfileModioUrls(activeProfile.id);
    }

    /**
     * 获取指定 profile 中所有 mod.io 类型的 mod URL 列表
     */
    public async getProfileModioUrls(profileId: number): Promise<string[]> {
        const profilesApi = await StorageAPI.getProfiles();
        const modsApi = await StorageAPI.getMods();

        // 1. 获取 profile 的所有 mod 关联
        const profileMods = await profilesApi.getProfileMods(profileId);

        if (profileMods.length === 0) {
            return [];
        }

        // 2. 批量获取 mod 信息
        const modIds = profileMods.map(pm => pm.modId);
        const modDataList = await modsApi.getBatchCompleteModDataOptimized(modIds);

        // 3. 过滤并提取 URL
        const urls: string[] = [];
        for (const mod of modDataList) {
            if (mod.sourceType === 'Modio' && mod.url) {
                urls.push(mod.url);
            }
        }

        return urls;
    }
}
```

**Service 层的优势**:
- 数据查询逻辑可以在多个 ViewModel 中复用
- 便于单元测试（不依赖 UI）
- 清晰的业务逻辑边界

---

### 4. DAO 层 (Data Access Object)

**位置**: `src/storage/dao/`

**职责**:
- ✅ 直接操作数据库（CRUD）
- ✅ SQL 查询封装
- ✅ 数据库事务管理
- ✅ 数据映射（数据库记录 → TypeScript 对象）

**禁止**:
- ❌ 业务逻辑处理
- ❌ UI 交互
- ❌ 跨表的复杂业务逻辑（应在 Service 层）

**示例**:
```typescript
// src/storage/dao/ProfileDAO.ts

export class ProfileDAO {
    /**
     * 获取活跃的配置文件
     */
    public async getActiveProfile(): Promise<ProfileData | null> {
        const db = await getDb();

        const games = await StorageAPI.getGames();
        const users = await StorageAPI.getUsers();

        const activeGame = await games.getActiveGame();
        const activeUser = await users.getActiveUser();

        if (!activeGame || !activeUser) {
            return null;
        }

        const result = await db.select().from(profiles)
            .where(and(
                eq(profiles.userId, activeUser.id!),
                eq(profiles.gameId, activeGame.id!),
                eq(profiles.isActive, true)
            ))
            .limit(1);

        return result.length > 0 ? this.mapToProfileData(result[0]) : null;
    }

    /**
     * 获取配置文件的所有模组关联
     */
    public async getProfileMods(profileId: number): Promise<ProfileModData[]> {
        const db = await getDb();
        const result = await db.select().from(profileMods)
            .where(eq(profileMods.profileId, profileId))
            .orderBy(profileMods.sortOrder);
        return result.map(this.mapToProfileModData);
    }
}
```

---

## 数据流向示例

### 场景：复制当前 profile 的 mod.io URL 列表

```
用户点击按钮
    ↓
┌──────────────────────────────────────┐
│ View: HomePage                        │
│ onMenuBarCopyListClick()              │
│   → 调用 HomeViewModel                │
└──────────────┬───────────────────────┘
               ↓
┌──────────────────────────────────────┐
│ ViewModel: HomeViewModel              │
│ exportModioUrlsToClipboard()          │
│   → 调用 ProfileService               │
│   → 显示成功/失败消息                  │
└──────────────┬───────────────────────┘
               ↓
┌──────────────────────────────────────┐
│ Service: ProfileService               │
│ getActiveProfileModioUrls()           │
│   → 调用 ProfileDAO                   │
│   → 调用 ModDAO                       │
│   → 聚合数据并过滤                    │
└──────────────┬───────────────────────┘
               ↓
┌──────────────────────────────────────┐
│ DAO: ProfileDAO & ModDAO              │
│   → 执行数据库查询                    │
│   → 返回原始数据                      │
└──────────────┬───────────────────────┘
               ↓
┌──────────────────────────────────────┐
│ Database: SQLite                      │
│   profiles, profile_mods, mods        │
└──────────────────────────────────────┘
```

---

## 分层原则和最佳实践

### ✅ 应该做的

1. **单向依赖**: 上层可以调用下层，下层不能调用上层
   - View → ViewModel → Service → DAO → Database

2. **职责单一**: 每层专注自己的职责
   - View: UI 渲染
   - ViewModel: 业务逻辑 + UI 交互
   - Service: 数据聚合 + 业务规则
   - DAO: 数据库操作

3. **接口清晰**: 每层提供清晰的公共接口
   ```typescript
   // ViewModel 提供给 View 的接口
   public async exportModioUrlsToClipboard(): Promise<boolean>

   // Service 提供给 ViewModel 的接口
   public async getActiveProfileModioUrls(): Promise<string[]>

   // DAO 提供给 Service 的接口
   public async getProfileMods(profileId: number): Promise<ProfileModData[]>
   ```

4. **错误处理分层**:
   - DAO: 抛出数据库异常
   - Service: 处理业务异常，返回结果或 null
   - ViewModel: 处理异常并显示用户友好的错误消息
   - View: 展示错误状态

### ❌ 不应该做的

1. **跨层调用**: View 直接调用 Service 或 DAO
   ```typescript
   // ❌ 错误示例
   class HomePage {
       async onClick() {
           const service = new ModService();
           await service.addMod(...); // 应该通过 ViewModel
       }
   }
   ```

2. **循环依赖**: Service 调用 ViewModel，ViewModel 调用 Service
   ```typescript
   // ❌ 错误示例
   class ProfileService {
       async doSomething() {
           const vm = await IoC.get(ProfileViewModel); // 不应该调用上层
       }
   }
   ```

3. **职责混乱**: DAO 包含业务逻辑，Service 包含 UI 交互
   ```typescript
   // ❌ 错误示例
   class ModDAO {
       async addMod(mod: ModData) {
           const result = await db.insert(mods).values(mod);
           message.success("添加成功"); // DAO 不应该有 UI 交互
           return result;
       }
   }
   ```

---

## 重构指南

当你需要重构现有代码时，按照以下步骤：

### 步骤 1: 识别职责
- 这段代码是在做什么？
  - 数据库查询 → DAO
  - 数据聚合/业务规则 → Service
  - UI 交互/业务逻辑 → ViewModel
  - UI 渲染 → View

### 步骤 2: 提取到合适的层
```typescript
// 重构前：所有逻辑在 View 中
class HomePage {
    async onCopyClick() {
        const profilesApi = await StorageAPI.getProfiles();
        const activeProfile = await profilesApi.getActiveProfile();
        const profileMods = await profilesApi.getProfileMods(activeProfile.id);
        const modIds = profileMods.map(pm => pm.modId);
        const modsApi = await StorageAPI.getMods();
        const modDataList = await modsApi.getBatchCompleteModDataOptimized(modIds);
        let list = "";
        for (const mod of modDataList) {
            if (mod.sourceType === 'Modio' && mod.url) {
                list += mod.url + "\n";
            }
        }
        await navigator.clipboard.writeText(list);
        message.success(t("Copied"));
    }
}

// 重构后：职责分层
// 1. Service 层：数据查询
class ProfileService {
    async getActiveProfileModioUrls(): Promise<string[]> {
        const profilesApi = await StorageAPI.getProfiles();
        const activeProfile = await profilesApi.getActiveProfile();
        const profileMods = await profilesApi.getProfileMods(activeProfile.id);
        const modIds = profileMods.map(pm => pm.modId);
        const modsApi = await StorageAPI.getMods();
        const modDataList = await modsApi.getBatchCompleteModDataOptimized(modIds);
        return modDataList
            .filter(mod => mod.sourceType === 'Modio' && mod.url)
            .map(mod => mod.url!);
    }
}

// 2. ViewModel 层：业务逻辑 + UI 交互
class HomeViewModel {
    async exportModioUrlsToClipboard(): Promise<boolean> {
        const service = new ProfileService();
        const urls = await service.getActiveProfileModioUrls();

        if (urls.length === 0) {
            message.warning(t("No mods found"));
            return false;
        }

        const list = urls.join("\n") + "\n";
        await navigator.clipboard.writeText(list);
        message.success(t("Copied"));
        return true;
    }
}

// 3. View 层：调用 ViewModel
class HomePage {
    async onCopyClick() {
        const vm = await IoC.get(HomeViewModel);
        await vm.exportModioUrlsToClipboard();
    }
}
```

### 步骤 3: 验证重构
- ✅ 每层职责清晰
- ✅ 依赖方向正确（单向向下）
- ✅ 代码可复用
- ✅ 易于测试

---

## 常见场景和分层示例

### 场景 1: 添加 Mod

```typescript
// View
@autoBind
private async onAddModClick() {
    const vm = await IoC.get(HomeViewModel);
    await vm.addModFromUrl(url, folderId);
}

// ViewModel
public async addModFromUrl(url: string, folderId: number): Promise<boolean> {
    const modInfo = await ModioApi.getModInfoByLink(url);
    const addedMod = await ModService.addModFromModio(modInfo, profileId, folderId);

    if (modInfo.dependencies) {
        await this.addModDependencies(modInfo.id, folderId);
    }

    TreeViewModel.updateTreeView();
    message.success(t("Mod added"));
    return true;
}

// Service
export class ModService {
    public static async addModFromModio(
        modInfo: ModioModInfo,
        profileId: number,
        folderId: number
    ): Promise<CompleteModData> {
        const modsApi = await StorageAPI.getMods();
        const profilesApi = await StorageAPI.getProfiles();

        // 创建 mod 记录
        const modData = ModMapper.fromModioToModData(modInfo);
        const createdMod = await modsApi.createMod(modData);

        // 关联到 profile
        await profilesApi.addModToProfile({
            profileId,
            modId: createdMod.modId!,
            parentFolderId: folderId,
            isEnabled: true
        });

        return await modsApi.getCompleteModData(createdMod.modId!);
    }
}

// DAO
public async createMod(modData: ModData): Promise<ModData | null> {
    const db = await getDb();
    const result = await db.insert(mods).values(modData).returning();
    return result.length > 0 ? this.mapToModData(result[0]) : null;
}
```

### 场景 2: 删除 Mod

```typescript
// View
@autoBind
private async onDeleteClick(modId: number) {
    const vm = await IoC.get(HomeViewModel);
    await vm.removeMod(modId);
}

// ViewModel
public async removeMod(modId: number): Promise<void> {
    const profiles = await StorageAPI.getProfiles();
    const activeProfile = await profiles.getActiveProfile();

    await profiles.removeModFromProfile(activeProfile.id!, modId);

    TreeViewModel.updateTreeView();
    TreeViewModel.updateTreeViewCountLabel();
    message.success(t("Mod removed"));
}

// DAO
public async removeModFromProfile(profileId: number, modId: number): Promise<boolean> {
    const db = await getDb();
    await db.delete(profileMods)
        .where(and(
            eq(profileMods.profileId, profileId),
            eq(profileMods.modId, modId)
        ));
    return true;
}
```

### 场景 3: 获取 Mod 列表

```typescript
// View
async componentDidMount() {
    const vm = await IoC.get(TreeViewModel);
    const treeData = await vm.getTreeData();
    this.setState({ treeData });
}

// ViewModel
public async getTreeData(): Promise<ProfileTreeItem> {
    const service = new ProfileService();
    return await service.getActiveProfileTreeRoot();
}

// Service
public async getActiveProfileTreeRoot(): Promise<ProfileTreeItem> {
    const activeProfile = await this.ensureActiveProfile();
    return await this.treeService.loadProfileTreeRoot(activeProfile);
}

// DAO (通过 TreeService 调用)
public async getProfileTree(profileId: number): Promise<ProfileTreeData | null> {
    const profile = await this.getProfileById(profileId);
    const [folders, mods] = await Promise.all([
        this.getProfileFolders(profileId),
        this.getProfileMods(profileId)
    ]);

    return {
        ...profile,
        folders: this.buildFolderTree(folders, mods),
        mods: rootMods
    };
}
```

---

## 总结

MintCat 的分层架构确保了：

1. **职责分离**: 每层专注于自己的职责
2. **可维护性**: 代码组织清晰，易于理解和修改
3. **可复用性**: Service 层的方法可以被多个 ViewModel 复用
4. **可测试性**: 每层可以独立测试
5. **扩展性**: 添加新功能时，明确知道代码应该放在哪一层

遵循这些原则，可以保持代码库的健康和可维护性。
