# 时间戳处理规范

> 历史规范：本文中的旧 `src/utils/TimeUtils.ts`、TypeScript 服务和兼容方法已随根目录旧源码清理移除，不再是当前依赖。当前各 CommandSpec/schema 声明的时间字段单位是合约权威；不通过恢复旧工具类或别名统一新运行时。当前边界见 [模块化运行时架构](modular-runtime-architecture.md)。

## 概述

本文档定义了 MintCat 项目中统一的时间戳处理规范，解决了历史代码中时间单位混乱、转换分散、缺乏类型安全等问题。

**版本**: 1.0
**最后更新**: 2025-12-25
**适用范围**: 所有涉及时间戳的前端和后端代码

---

## 1. 设计原则

### 1.1 核心原则

| 原则 | 说明 |
|-----|------|
| **统一存储单位** | 数据库中所有时间戳统一使用**毫秒**存储 |
| **集中转换逻辑** | 所有单位转换集中在 `TimeUtils` 工具类 |
| **类型标识** | 使用 `TimestampMs` 和 `TimestampSec` 类型标识提高可读性 |
| **封装业务逻辑** | 常见的时间比较逻辑封装为工具方法 |
| **向后兼容** | 旧方法标记为 `@deprecated` 但保持可用 |

### 1.2 时间单位约定

| 来源 | 单位 | 说明 | 示例值 |
|-----|------|------|--------|
| **JavaScript 标准** | 毫秒 | `Date.now()`、`fileInfo.mtime.getTime()` | `1735123456789` |
| **Unix 标准** | 秒 | mod.io API、系统时间戳 | `1735123456` |
| **数据库存储** | 毫秒 | 所有业务时间戳字段 | `1735123456789` |
| **Drizzle timestamp mode** | 秒 | `createdAt`、`updatedAt` 字段 | `1735123456` |

---

## 2. TimeUtils 工具类

### 2.1 类型定义

```typescript
/**
 * 时间戳类型标识（用于文档和注释）
 * - TimestampMs: 毫秒时间戳 (JavaScript 标准)
 * - TimestampSec: 秒时间戳 (Unix 标准)
 */
export type TimestampMs = number;
export type TimestampSec = number;
```

### 2.2 API 速查表

| 方法 | 用途 | 返回类型 | 示例 |
|-----|------|---------|------|
| `TimeUtils.now()` | 获取当前时间（毫秒）⭐ | `TimestampMs` | `1735123456789` |
| `TimeUtils.nowSeconds()` | 获取当前时间（秒） | `TimestampSec` | `1735123456` |
| `TimeUtils.fromModio(sec)` | mod.io API 秒转毫秒⭐ | `TimestampMs` | `fromModio(1735123456)` |
| `TimeUtils.toModio(ms)` | 毫秒转 mod.io API 秒 | `TimestampSec` | `toModio(1735123456789)` |
| `TimeUtils.secondsToMs(sec)` | 通用秒转毫秒 | `TimestampMs` | `secondsToMs(3600)` |
| `TimeUtils.msToSeconds(ms)` | 通用毫秒转秒 | `TimestampSec` | `msToSeconds(3600000)` |
| `TimeUtils.hasUpdate(online, local)` | 判断是否有更新⭐ | `boolean` | `hasUpdate(1000, 900)` |
| `TimeUtils.isNewer(a, b)` | 比较时间新旧 | `boolean` | `isNewer(1000, 900)` |
| `TimeUtils.formatTimestamp(ms)` | 格式化显示 | `string` | `"2025/12/25 10:30:45"` |

⭐ = 最常用方法

---

## 3. 数据库时间戳字段

### 3.1 业务时间戳字段（毫秒）

这些字段存储业务相关的时间戳，**统一使用毫秒**：

| 表名 | 字段名 | 说明 | 单位 |
|-----|--------|------|------|
| `mod_status` | `lastUpdateDate` | 本地文件最后更新时间 | 毫秒 |
| `mod_status` | `onlineUpdateDate` | mod.io 在线版本更新时间 | 毫秒 |
| `profiles` | `lastUsedAt` | 配置最后使用时间 | 秒（Drizzle timestamp） |

**定义示例：**
```typescript
export const modStatus = sqliteTable("mod_status", {
    modId: integer("mod_id").primaryKey(),
    lastUpdateDate: integer("last_update_date").notNull().default(0),      // 毫秒
    onlineUpdateDate: integer("online_update_date").notNull().default(0),  // 毫秒
    // ...
});
```

### 3.2 系统时间戳字段（秒）

这些字段由 Drizzle ORM 自动管理，**使用秒**：

| 字段名 | 说明 | 模式 |
|--------|------|------|
| `createdAt` | 记录创建时间 | `{ mode: 'timestamp' }` |
| `updatedAt` | 记录更新时间 | `{ mode: 'timestamp' }` |

**定义示例：**
```typescript
createdAt: integer("created_at", { mode: 'timestamp' })
    .notNull()
    .default(sql`(unixepoch())`),
```

---

## 4. 常见场景与代码示例

### 4.1 从 mod.io API 获取时间戳

**场景**: mod.io API 返回的时间戳是**秒**，需要转换为**毫秒**存储。

```typescript
// ❌ 错误写法
const onlineUpdateDate = modInfo.date_updated ? modInfo.date_updated * 1000 : Date.now();

// ✅ 正确写法
const onlineUpdateDate = TimeUtils.fromModio(modInfo.date_updated) || TimeUtils.now();
```

**处理事件时间：**
```typescript
// ❌ 错误写法
await modsApi.upsertModStatus({
    modId: mod.modId!,
    onlineUpdateDate: event.date_added * 1000,
    lastUpdateDate: 0
});

// ✅ 正确写法
await modsApi.upsertModStatus({
    modId: mod.modId!,
    onlineUpdateDate: TimeUtils.fromModio(event.date_added),
    lastUpdateDate: 0
});
```

### 4.2 调用 mod.io API 传递时间参数

**场景**: 调用 mod.io API 时需要传递**秒**级时间戳。

```typescript
// ❌ 错误写法
const updateTime = Math.floor(Date.now() / 1000);
const events = await ModioApi.getEvents(updateTime, modIds);

// ✅ 正确写法
const updateTime = TimeUtils.nowSeconds();
const events = await ModioApi.getEvents(updateTime, modIds);
```

**使用存储的毫秒时间戳：**
```typescript
// 从数据库读取的时间是毫秒
const lastUpdate = await profileVM.getActiveProfileLastUpdate(); // 秒

// 传递给 mod.io API（如果 lastUpdate 已经是秒，则直接使用）
const updateTime = lastUpdate || (TimeUtils.nowSeconds() - 60 * 60 * 24 * 30);
const events = await ModioApi.getEvents(updateTime, modIds);
```

### 4.3 获取文件修改时间

**场景**: 文件系统返回的 `mtime` 是 JavaScript `Date` 对象，调用 `.getTime()` 返回**毫秒**。

```typescript
import { stat } from "@tauri-apps/plugin-fs";

// 正确：fileInfo.mtime.getTime() 返回毫秒
const fileInfo = await stat(modPath);
const mtime = fileInfo.mtime.getTime(); // 毫秒

await modsApi.upsertModStatus({
    modId: modId,
    lastUpdateDate: mtime  // 直接存储毫秒
});
```

### 4.4 判断是否有更新

**场景**: 比较在线版本和本地版本的时间戳。

```typescript
// ❌ 错误写法（手动比较，容易遗漏 localDate > 0 检查）
const hasUpdate = data.lastUpdateDate > 0 &&
                  data.onlineUpdateDate > data.lastUpdateDate;

// ✅ 正确写法（使用封装的方法）
const hasUpdate = TimeUtils.hasUpdate(
    data.onlineUpdateDate,
    data.lastUpdateDate
);
```

**`hasUpdate` 方法的智能逻辑：**
```typescript
public static hasUpdate(onlineDate: TimestampMs, localDate: TimestampMs): boolean {
    // localDate 为 0 表示初始数据，不算有更新
    return localDate > 0 && onlineDate > localDate;
}
```

### 4.5 初始化时间戳

**场景**: 创建新的 mod 记录时初始化时间戳。

```typescript
// ❌ 错误写法
status: {
    modId: 0,
    lastUpdateDate: Date.now(),
    onlineUpdateDate: Date.now(),
    // ...
}

// ✅ 正确写法
status: {
    modId: 0,
    lastUpdateDate: TimeUtils.now(),
    onlineUpdateDate: TimeUtils.now(),
    // ...
}
```

**本地 mod 初始化：**
```typescript
status: {
    modId: 0,
    lastUpdateDate: TimeUtils.now(),
    onlineUpdateDate: 0,  // 本地 mod 无在线版本
    isOnlineAvailable: false,
    // ...
}
```

### 4.6 格式化时间显示

**场景**: 在 UI 中显示时间戳。

```typescript
// 毫秒时间戳格式化
const displayTime = TimeUtils.formatTimestamp(mod.status.lastUpdateDate);
// 输出: "2025/12/25 10:30:45"

// Date 对象格式化
const displayTime = TimeUtils.formatDate(new Date());
// 输出: "2025/12/25 10:30:45"
```

---

## 5. 核心文件修改记录

以下文件已按照本规范重构：

| 文件路径 | 修改内容 | 行号 |
|---------|---------|------|
| `src/utils/TimeUtils.ts` | 新增完整工具方法 | 全文 |
| `src/apis/ModUpdateService.ts` | 使用 `fromModio()` 转换 | 79, 268, 283 |
| `src/apis/ModUpdateService.ts` | 使用 `now()` 替代 `Date.now()` | 125 |
| `src/apis/ModUpdateService.ts` | 使用 `nowSeconds()` 获取秒时间戳 | 247, 292 |
| `src/pages/HomePage/TreeViewItem.tsx` | 使用 `hasUpdate()` 判断更新 | 178-181 |
| `src/utils/modHelpers.ts` | 使用 `hasUpdate()` 判断更新 | 54 |
| `src/mappers/ModMapper.ts` | 使用 `now()` 初始化时间 | 53, 54, 93 |
| `src/pages/HomePage/HomeViewModel.ts` | 添加注释说明单位 | 193 |

---

## 6. 迁移指南

### 6.1 查找需要迁移的代码

使用以下 grep 命令查找可能需要迁移的代码：

```bash
# 查找手动转换的代码
grep -rn "\* 1000" src/ --include="*.ts" --include="*.tsx"
grep -rn "/ 1000" src/ --include="*.ts" --include="*.tsx"

# 查找 Date.now() 的使用
grep -rn "Date.now()" src/ --include="*.ts" --include="*.tsx"

# 查找 Math.floor(Date.now() / 1000) 的使用
grep -rn "Math.floor(Date.now()" src/ --include="*.ts" --include="*.tsx"
```

### 6.2 迁移检查清单

- [ ] 所有 mod.io API 时间戳转换使用 `TimeUtils.fromModio()`
- [ ] 所有 `Date.now()` 替换为 `TimeUtils.now()`
- [ ] 所有 `Math.floor(Date.now() / 1000)` 替换为 `TimeUtils.nowSeconds()`
- [ ] 所有时间比较使用 `TimeUtils.hasUpdate()` 或 `TimeUtils.isNewer()`
- [ ] 所有手动 `* 1000` 或 `/ 1000` 使用工具方法替代
- [ ] 在文件开头导入 `TimeUtils`

### 6.3 导入语句

```typescript
import { TimeUtils } from '@/utils/TimeUtils';
```

---

## 7. 最佳实践

### 7.1 DO（推荐做法）

✅ **使用类型标识增强可读性**
```typescript
function processTimestamp(timestamp: TimestampMs) {
    // 明确表示这是毫秒时间戳
}
```

✅ **使用语义化的工具方法**
```typescript
const currentTime = TimeUtils.now();
const hasUpdate = TimeUtils.hasUpdate(onlineDate, localDate);
```

✅ **添加单位注释**
```typescript
// fileInfo.mtime.getTime() returns milliseconds (JavaScript standard)
const mtime = fileInfo.mtime.getTime();
```

✅ **统一使用工具类**
```typescript
// 所有时间戳操作都通过 TimeUtils
const timestamp = TimeUtils.now();
const seconds = TimeUtils.toModio(timestamp);
```

### 7.2 DON'T（避免的做法）

❌ **不要手动转换时间单位**
```typescript
// 不要这样做
const seconds = Math.floor(timestamp / 1000);
const milliseconds = seconds * 1000;

// 应该使用
const seconds = TimeUtils.msToSeconds(timestamp);
const milliseconds = TimeUtils.secondsToMs(seconds);
```

❌ **不要直接使用 Date.now()**
```typescript
// 不要这样做
const now = Date.now();

// 应该使用
const now = TimeUtils.now();
```

❌ **不要重复实现比较逻辑**
```typescript
// 不要这样做
if (data.lastUpdateDate > 0 && data.onlineUpdateDate > data.lastUpdateDate) {
    // ...
}

// 应该使用
if (TimeUtils.hasUpdate(data.onlineUpdateDate, data.lastUpdateDate)) {
    // ...
}
```

❌ **不要混用时间单位**
```typescript
// 不要这样做（单位不一致）
if (timestampInSeconds > timestampInMilliseconds) { // 错误！
    // ...
}

// 应该统一单位后再比较
if (TimeUtils.secondsToMs(timestampInSeconds) > timestampInMilliseconds) {
    // ...
}
```

---

## 8. 常见问题 (FAQ)

### Q1: 为什么选择毫秒而不是秒作为存储单位？

**A**:
1. JavaScript 标准使用毫秒（`Date.now()`、文件系统 API）
2. 毫秒精度更高，适合前端应用
3. 只需要在与外部 API（mod.io）交互时转换

### Q2: 什么时候使用 `fromModio()` 而不是 `secondsToMs()`？

**A**:
- `fromModio()` 专门处理 mod.io API，包含 `undefined` 和 `0` 值的特殊处理
- `secondsToMs()` 是通用的单位转换，用于其他场景

```typescript
// fromModio 会处理 undefined/0 的情况
TimeUtils.fromModio(undefined) // 返回 0
TimeUtils.fromModio(0)         // 返回 0

// secondsToMs 是纯转换
TimeUtils.secondsToMs(0)       // 返回 0
TimeUtils.secondsToMs(undefined) // 类型错误！
```

### Q3: `hasUpdate()` 为什么要检查 `localDate > 0`？

**A**:
- `localDate === 0` 表示初始数据或从未下载过
- 这种情况不应该显示"有新版本"的警告
- 封装在 `hasUpdate()` 中避免每次都要手动检查

### Q4: Drizzle timestamp 模式的字段怎么处理？

**A**:
- `createdAt` 和 `updatedAt` 由 Drizzle ORM 自动管理，使用秒
- 这些是系统字段，不需要手动转换
- 业务逻辑应该使用其他字段（如 `lastUpdateDate`）

### Q5: 旧代码中的 `getCurrentTime()` 还能用吗？

**A**:
- 可以，为了向后兼容保留了旧方法
- 但标记为 `@deprecated`，建议逐步迁移到新方法
- `getCurrentTime()` → `nowSeconds()`
- `getTimeSecond()` → `msToSeconds()`

---

## 9. 测试建议

### 9.1 单元测试示例

```typescript
describe('TimeUtils', () => {
    it('should convert seconds to milliseconds', () => {
        expect(TimeUtils.secondsToMs(1)).toBe(1000);
        expect(TimeUtils.secondsToMs(0)).toBe(0);
    });

    it('should convert milliseconds to seconds', () => {
        expect(TimeUtils.msToSeconds(1000)).toBe(1);
        expect(TimeUtils.msToSeconds(1999)).toBe(1); // 向下取整
    });

    it('should detect updates correctly', () => {
        expect(TimeUtils.hasUpdate(1000, 900)).toBe(true);  // 有更新
        expect(TimeUtils.hasUpdate(900, 1000)).toBe(false); // 无更新
        expect(TimeUtils.hasUpdate(1000, 0)).toBe(false);   // 初始数据
    });

    it('should handle mod.io timestamp conversion', () => {
        expect(TimeUtils.fromModio(1735123456)).toBe(1735123456000);
        expect(TimeUtils.fromModio(undefined)).toBe(0);
        expect(TimeUtils.fromModio(0)).toBe(0);
    });
});
```

### 9.2 集成测试场景

- 测试 mod.io API 时间戳正确转换并存储
- 测试文件修改时间正确记录
- 测试更新检测逻辑正确触发
- 测试时间显示格式正确

---

## 10. 附录

### 10.1 相关文档

- [Mod Version Update Business Logic](./mod-version-update-logic.md)
- [Architecture Layers](./architecture-layers.md)

### 10.2 外部参考

- [mod.io API Documentation](https://docs.mod.io/)
- [JavaScript Date MDN](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Date)
- [Unix Time - Wikipedia](https://en.wikipedia.org/wiki/Unix_time)

### 10.3 变更历史

| 版本 | 日期 | 变更内容 |
|-----|------|---------|
| 1.0 | 2025-12-25 | 初始版本，完成时间戳规范统一 |

---

**维护者**: MintCat 开发团队
**最后审阅**: 2025-12-25
