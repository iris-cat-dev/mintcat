---
name: mintcat-release-version
description: 将 MintCat 前端与 Tauri 的版本号对齐，维护 Changelog.md 与 latest.json（含 notes）；打包需本地输入密码；signature 须在构建生成 .sig 后写入 latest.json。在用户要求发版、升级版本号、更新更新说明或执行 release/package 时使用。
---

# MintCat 版本更新与打包

## 目标版本

由用户或对话明确 **目标版本号**（如 `0.5.3` 或 `0.5.3-beta.1`）。以下四处 **字符串必须一致**：

| 文件 | 字段 |
|------|------|
| `package.json` | `"version"` |
| `src-tauri/tauri.conf.json` | `"version"` |
| `src-tauri/Cargo.toml` | `[package]` → `version` |
| `latest.json` | 顶层 `"version"` |

编辑时用精确替换，避免误改依赖里的 `"version"` 字段（仅改上述位置）。

## Changelog.md

- 路径：仓库根目录 `Changelog.md`（若无则新建）。
- 内容：本次版本的变更说明；项目惯例为 **中文 + English** 分段（参考既有 `latest.json` 的 `notes` 结构：标题层级、新增/优化/修复 等）。
- 发版完成后，该文件即为本版本的**权威**变更记录。

## latest.json

在 `Changelog.md` 定稿后：

1. 将 **`Changelog.md` 的完整正文**（Markdown 字符串）写入 `latest.json` 的 **`"notes"`** 字段。JSON 中需正确转义换行与引号（`\n`、`\"`）。
2. 将 **`"version"`** 设为目标版本（与三处版本号一致）。
3. 将 **`"pub_date"`** 更新为本次发版的 ISO 8601 时间，例如 `2026-03-31T00:00:00.000Z`（按实际发版日）。
4. **`platforms`**：
   - **`url`**：指向本次 Release 的安装包（版本号与文件名需与实际上传一致）；可在定稿 `notes` / `version` 时先写好。
   - **`signature`**：**必须在 Windows 安装包构建完成并生成 `.sig` 之后**再填入。构建前**不要**把旧版本的签名写进 `latest.json` 冒充新版；也不要用占位符。正确流程见下文「执行顺序 checklist」。
   - 签名文件路径（交叉编译 Windows 时通常为）：`src-tauri/target/x86_64-pc-windows-gnu/release/bundle/nsis/mintcat_<版本>_x64-setup.exe.sig`。将文件**完整单行 Base64 内容**（与 `.sig` 文件内容一致）作为 `signature` 字符串写入 JSON。

仓库内 `latest.json` 的 Release 链接与 compare 链接格式可参考当前文件；若需 **Full Changelog** 的 GitHub compare，将 `v旧版本...v新版本` 与仓库实际 owner/repo 对齐。

## 打包

在项目根目录执行：

```bash
npm run package
```

等价于 `./scripts/build-release.sh`：使用 `cargo-xwin` 交叉编译 Windows 目标 `x86_64-pc-windows-gnu`。需已安装 Rust、Tauri CLI、pnpm，且按需配置 **`TAURI_SIGNING_PRIVATE_KEY`**（或通过 `TAURI_KEY_FILE` / 默认 `~/.tauri/mintcat.key` 读取私钥），详见 `scripts/build-release.sh`。

**打包过程中的密码**：构建/签名阶段若终端提示输入密码，须由操作者在本地输入（例如私钥或密钥相关口令）。自动化或 Agent 无法代替输入；应在 skill / 对话中说明「需在终端手动输入密码」，不要假定无需交互即可完成打包。

若打包失败，根据终端错误排查（网络、toolchain、签名密钥、密码错误等），不要假定脚本已静默成功。

## 执行顺序 checklist

1. 确认目标版本号。
2. 更新 `package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 中的版本。
3. 编写或更新 `Changelog.md`。
4. 将 `Changelog.md` 正文同步到 `latest.json` 的 `notes`，并更新 `version`、`pub_date`；按需更新 `platforms` 中的 **`url`**。**不要**在尚未生成 `.sig` 时把旧版签名当作本版写入；最终 `signature` 仅在步骤 6 用构建产物填入（若需分步提交，可先改 `notes`/`version`/`url`，待打包后再补 `signature`）。
5. 运行 `npm run package`（过程中若提示输入密码，在本地终端手动输入）。
6. **构建成功后**，从 `src-tauri/target/x86_64-pc-windows-gnu/release/bundle/nsis/mintcat_<版本>_x64-setup.exe.sig` 读取内容，将 **`signature`** 写入 `latest.json`（与本次生成的 `.exe` 严格对应）。若上一步已暂存 `signature` 为旧版或占位，本步必须替换为新生成内容后再发布。

## 注意事项

- **不要**在未要求时修改 `package.json` 的 `dependencies` / `devDependencies` 版本。
- 若 `package.json` 与 Tauri 使用预发布标签（如 `-beta.1`），三处与 `latest.json` 仍须与本次发布约定一致。
- 发版提交前可建议用户 `npx tsc --noEmit` 快速类型检查（非 skill 强制步骤，按用户要求执行）。
