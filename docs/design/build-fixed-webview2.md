# WebView2 内嵌版（fixed-webview2）打包说明

当前入口是 `scripts/build-fixed-webview2.mjs`，委托统一的 `packageRelease('fixed-webview2')`，从已验签的不可变 stage 组装发行包。根目录旧 `src-tauri/`、旧 Windows 配置与编译缓存已移除；不再下载运行时到旧目录，也不切换或覆盖源码配置。

## 前置条件

- 在真实 Windows 主机上组装；目标必须为 `x86_64-pc-windows-msvc`，不支持旧 GNU target 或在 macOS 上冒充 Windows 安装验证。
- 先通过 `mintcat-module stage --manifest <release-input.json>` 创建固定 digest 的签名 stage；stage 必须含 CLI、daemon、模块验证器、已编译且嵌入离线入口的 Desktop、可信签名模块基线和固定 WebView2 资源。
- 显式设置 `MINTCAT_MODULE_BIN`，指向已经构建的原生 `mintcat-module` 验证器；打包器不隐式运行开发 Cargo 构建。
- production 模式要求有效原生 Authenticode 签名，并配置 `MINTCAT_WINDOWS_SIGN_COMMAND`。development 模式也必须验证签名模块基线，不能使用未签名业务模块作为回退。

## Assembly plan

通过 `--manifest` 提供 JSON 文件，字段为：

| 字段 | 要求 |
| --- | --- |
| `schemaVersion` | `1` |
| `target` | `x86_64-pc-windows-msvc` |
| `variant` | `fixed-webview2` |
| `mode` | 显式 `production` 或 `development` |
| `stage` | 不可变 stage 目录，相对于 plan 文件解析 |
| `stageManifestSha256` | 实际 stage manifest 的 `sha256:` + 64位十六进制 digest |
| `output` | 相对于 plan 文件解析的新输出路径，不得在 stage 内，不覆盖已有输出 |

```shell
pnpm package:fixed-webview2 --manifest /absolute/path/assembly.json
```

命令在已设置 `MINTCAT_MODULE_BIN` 和对应签名配置的环境中执行。不是旧 `pnpm build:fixed-webview2`，也不接受 `WEBVIEW2_VERSION` 代替固定 stage。

## 组装与产物

1. 在创建输出前验签 stage，校验目标、flavor、完整 scope/包闭包及 manifest digest。
2. 调用固定 Desktop 的 `--verify-embedded-ui`，拒绝依赖开发服务器或空 frontend 的 GUI。
3. 将 stage 保留在 `output/runtime/`；生成 output-local native bundling 配置，用 stage 的 `webview2` 目录配置 `fixedRuntime`。
4. 从 `apps/desktop` 调用 `tauri bundle`，使用固定 Desktop 字节，不重新构建或修改 stage。
5. 将真实平台产物保存在 `output/bundles/`，验证复制前后的字节、权限和符号链接；写入 `output/assembly-manifest.json`。

运行数据由 stage 的声明决定；CLI 和 GUI 必须连接同一个目录，`--data-dir` / `MINTCAT_DATA_DIR` 是显式覆盖，不隐式导入旧数据库。

## 验证边界

验签或打包成功不等于干净 Windows 主机的运行时/安装验收。Windows 签名、固定 WebView2、真实游戏资源和安装矩阵仍须实际目标主机验证；当前状态见 [执行验收记录](modular-refactor-todo.md)。
