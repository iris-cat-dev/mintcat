# Rust 后端多语言支持方案

> 历史方案：本文针对已退役的 Tauri command/renderer 日志与 `src/locales` 翻译文件；根目录旧源码和语言包现已清理。下文不是当前模块的修改指引。当前消息/error 合约与模块文本命名空间以 [模块化运行时架构](modular-runtime-architecture.md) 和实际 CommandSpec/schema 为准。

## 现状

后端当前直接发出或返回**英文硬编码字符串**，前端原样展示：

- **`status-bar-log`**：安装/下载进度文案（如 `"Installing UE4SS..."`、`"Process Mod: xxx Success"`）
- **`install-error`**：安装失败时把错误信息发给前端的 `app-error`
- **Tauri command 的 `Result<T, String>`**：如 `Err("Invalid game path: ...")`、`"Failed to parse mod list"` 等
- **DownloadError** 等：`Display` 实现为英文（`"Network error"`、`"Download cancelled"` 等）

## 推荐方案：后端发「消息 key」，前端用 i18n 翻译

不在 Rust 里做语言切换，而是：

1. **后端只发「消息 key」**（必要时带插值参数）
2. **前端用现有 i18n（`t(key)` / `t(key, params)`）** 显示文案
3. **翻译内容只维护在前端**（`en.json` / `zh-CN.json`），和现有多语言一致

这样：

- 翻译只维护一份（前端 locale）
- 不需要在 Rust 里引入 i18n 库或读配置
- 前端已有 `t()`，改动集中在前端展示层和少量后端 key 约定

---

## 1. 约定：后端消息 key 的格式

- **简单文案**：发一个字符串 key，例如 `"backend.install.start"`。
- **带插值的文案**：发一个对象，例如 `{ "key": "backend.install.process_mod_success", "name": "ModName" }`，前端用 `t(key, { name })`。
- key 建议统一前缀，例如 **`backend.`**，便于前端区分「需要翻译的 key」和「历史遗留的裸文案」。

---

## 2. 后端改动（Rust）

### 2.1 定义并只发 key（和可选参数）

把原来的英文文案改成发 key（和必要参数），例如：

```rust
// 之前
app.emit("status-bar-log", "Installing UE4SS...").unwrap();

// 之后：只发 key
app.emit("status-bar-log", "backend.install.ue4ss").unwrap();
```

带模组名时，可发结构化 payload（若前端支持对象）：

```rust
// 之前
app.emit("status-bar-log", format!("Process Mod: {} Success", mod_info.name)).unwrap();

// 之后：发 key + 参数（前端用 t(key, { name })）
#[derive(serde::Serialize)]
struct StatusLogPayload {
    key: &'static str,
    name: String,
}
app.emit("status-bar-log", StatusLogPayload {
    key: "backend.install.process_mod_success",
    name: mod_info.name.clone(),
}).unwrap();
```

或为省事仍发「带占位符的一条 key」，由前端用一条翻译 + 插值（见下）。

### 2.2 Command 错误返回 key

Tauri command 里尽量返回**错误 key**，而不是整句英文：

```rust
// 之前
.map_err(|e| format!("Invalid game path: {:#}", e))?;

// 之后：只返回 key，必要时把细节放到 params 或 log
.map_err(|_| "backend.error.invalid_game_path".to_string())?;
```

若需要把「动态信息」带给用户（如文件名），可以约定返回 JSON：  
`{"key": "backend.error.xxx", "path": "..."}`，前端用 `t(key, { path })`。

### 2.3 需要替换的字符串汇总（供你逐步改）

| 位置 | 当前字符串示例 | 建议 key |
|------|----------------|----------|
| drg/mod.rs | "Start Install...", "Load Mods ..." | backend.install.start, backend.install.load_mods |
| drg/pak_integrator.rs | "Installing UE4SS...", "Start Process Mod: {} ...", "Process Mod: {} Success", "Patch Game Pak...", "Write Mod...", "Install Mod Success" | backend.install.ue4ss, backend.install.process_mod_start, backend.install.process_mod_success, backend.install.patch_pak, backend.install.write_mod, backend.install.success |
| drgrc/pak_integrator.rs | 同上 + "Installing UE4SS..." 等 | 同上 |
| lib.rs / command | "Invalid path", "Invalid game path: ..." | backend.error.invalid_path, backend.error.invalid_game_path |
| capability/download | DownloadError 的 Display 文案 | 可在前端把常见错误映射成 key，见下 |

下载错误可以有两种做法：

- **A**：Rust 改为返回错误 key（如 `download.network_error`），前端 `t(key)`。
- **B**：Rust 仍返回英文短句，前端在显示前做一次「字符串 → key」的映射（例如 `"Network error: ..."` → `backend.download.network_error`），再 `t(key)`。这样 Rust 改动最小。

---

## 3. 前端改动

### 3.1 状态栏：对 `status-bar-log` 做翻译

在 `StatusBar.tsx` 里，收到 `status-bar-log` 时，若 payload 是 key（或带 key 的对象），则用 `t()` 再显示：

```ts
useEventListener('status-bar-log', (msg) => {
    let text: string;
    if (typeof msg === 'string') {
        text = msg.startsWith('backend.') ? t(msg) : msg;
    } else if (msg && typeof msg === 'object' && 'key' in msg && typeof (msg as { key: string }).key === 'string') {
        const { key, ...params } = msg as { key: string; [k: string]: unknown };
        text = t(key, params);
    } else {
        text = (msg as { message?: string })?.message ?? String(msg);
        text = text.startsWith('backend.') ? t(text) : text;
    }
    setMessage(text);
    // ... 其余逻辑不变
});
```

这样：

- 以 `backend.` 开头的字符串或带 `key` 的对象都会走 i18n
- 旧的不带 key 的字符串仍按原样显示（兼容过渡期）

### 3.2 安装错误：`install-error` / `app-error` 展示时翻译

在展示 `app-error` 的地方（例如弹窗或全局错误提示）对消息做一次翻译：

```ts
// 例如
const displayMessage = typeof errorMsg === 'string' && errorMsg.startsWith('backend.')
  ? t(errorMsg)
  : (t(errorMsg) !== errorMsg ? t(errorMsg) : errorMsg); // 若 key 存在则用翻译，否则用原文
```

这样后端逐步改成只发 `backend.xxx` 的 key 后，用户就会看到当前语言的错误提示。

### 3.3 在 locale 中增加 backend key

在 `src/locales/en.json` 和 `src/locales/zh-CN.json` 中为上述 key 补翻译，例如：

```json
{
  "backend.install.start": "Start Install...",
  "backend.install.load_mods": "Load Mods ...",
  "backend.install.ue4ss": "Installing UE4SS...",
  "backend.install.process_mod_success": "Process Mod: {{name}} Success",
  "backend.install.write_mod": "Write Mod...",
  "backend.install.success": "Install Mod Success",
  "backend.error.invalid_game_path": "Invalid game path"
}
```

中文对应条目在 zh-CN 里写「正在安装 UE4SS...」「模组处理成功：{{name}}」等即可。

---

## 4. 可选：带参数的 payload 的约定

若后端发对象，建议统一形状，便于前端一处处理：

```ts
// 前端可约定
type StatusBarLogPayload =
  | string
  | { message: string; level?: 'info' | 'success' | 'warning' | 'error' }
  | { key: string; [k: string]: unknown };  // key + 插值参数
```

Rust 端用 `serde::Serialize` 结构体或 `serde_json::json!` 发出即可。

---

## 5. 实施顺序建议

1. 在 **en.json / zh-CN.json** 里先加齐所有 `backend.*` 的 key（与现有后端字符串一一对应）。
2. **前端**：先改 StatusBar 和 app-error 展示逻辑，对 `backend.` 或 `{ key }` 做 `t()`；未改后端前仍收到英文时，可保留「非 key 则原样显示」的兼容。
3. **后端**：按文件逐步把 `emit("status-bar-log", ...)` 和 `Result<_, String>` 的 `Err(...)` 改成发/返回 key（和可选参数）。
4. 最后再考虑 **DownloadError** 等：要么在 Rust 里改成返回 key，要么在前端做「错误文案 → key」的映射并用 `t(key)` 显示。

按上述方式，Rust 端只负责发「消息 key + 少量参数」，所有多语言逻辑和文案都保留在前端，与现有 `t()` 体系一致。
