# Mod 下载、数据提交与列表编辑的原子能力锁

状态：已实现。R14 提供共享 Transfer/attempt/consumer、校验与 artifact 发布；R15 提供固定 DownloadOperation、cache/apply 分离、持久 job/action、fence 条件提交、取消与恢复；R16 已把列表入口切换到事务一致的 capability 与身份化 transfer overlay。

用户已确认现有问题：下载期间会更新 Mod 数据，而用户仍可编辑列表，缺少原子能力锁导致数据错乱。将此作为已知问题处理，不要求重新复现后才接受。

## 1. 结论

不能仅增加 `isDownloading` 或禁用一个按钮。采用五层约束：

1. **领域数据分开**：线上最新版本、Profile 选择、已缓存文件、下载任务、已安装结果是五件不同的事。
2. **后端原子能力锁**：检查 revision、取得逻辑租约、记录操作意图在数据所有者的同一个短事务内完成；全部 GUI/CLI/Agent 写入口都经过它。
3. **不可变下载输入**：任务固定 source/game/mod/version/file 身份，不在完成时重新读取“当前游戏/当前版本”。
4. **条件提交**：成功完成下载不等于有权修改当前列表；还必须匹配 operation、fencing token、目标 incarnation 和 contentRevision。
5. **增量事件**：下载进度不再携带并覆盖一份旧 `CompleteModData`；下载 overlay 与领域列表分别更新。

默认产品策略：**“下载并更新当前配置”期间，对该 Profile 的内容编辑取得排他逻辑锁；其他 Profile、搜索、查看、显示名编辑不被全局冻结。** 单纯预下载到缓存不修改 Profile，因此不锁列表。

这先保证正确性和可解释性，不在第一轮重构中实现复杂的逐行读写锁图。将来若要缩小为行级锁，必须保持本文所有提交不变量，不能只解禁 UI。

## 2. 当前代码证据

| 当前实现 | 位置 | 对应问题 |
| --- | --- | --- |
| 进度事件展开启动时的 `mod` 快照，再作为完整 data 发出 | `src/services/ModUpdateService.ts:53-88,206-227` | 进度与领域数据混合，缺少 version/attempt/profile 身份 |
| 下载前写 version/download，完成后又分别写 download/version/status | `ModUpdateService.ts:604-674` | 没有包含整个结果的原子提交与前置 revision 校验 |
| 完成时以 `modId` 更新全局 currentVersion/cachePath | `ModUpdateService.ts:650-664` | 同一 Mod 不同版本、不同 Profile 的结果不能可靠区分 |
| 完成时读取此刻 onlineUpdateDate，再写 lastUpdateDate | `ModUpdateService.ts:667-673` | `[INFERENCE]` 下载旧版本时线上元数据又更新，可能把旧文件标成最新 |
| 元数据更新也修改 currentVersion、downloadUrl、fileSize | `ModUpdateService.ts:347-374` | “线上发现新版本”与“本地已有某版本”共用状态 |
| DAO 先查再 upsert，多个表分别提交 | `src/storage/dao/ModDAO.ts:290-311,343-371` | 不是带业务前置条件的下载结果事务 |
| 版本切换有组件侧处理锁，但先写 usedVersion 再下载 | `src/pages/HomePage/TreeViewItem.tsx:487-523,560-586` | 已有局部锁不能覆盖列表编辑、其他客户端和服务端回调；失败时选择与文件可能脱节 |
| 下载进度缓存和事件过滤主要按 modId | `TreeViewItem.tsx:139-149,791-809` | 无法区分两个 Profile 使用同一 Mod 的不同文件版本 |
| 删除、启停、版本、移动走独立 DAO 更新 | `src/services/HomeService.ts:413-469` | 没有与下载操作共享的后端互斥协议 |

这里不把“没有任何锁”当作事实：版本选择组件已有局部锁。缺的是**跨入口、跨任务、跨数据写入路径的原子能力门禁和提交协议**。

底层 `DownloadApi` 已有 downloadId 和取消接口，可以保留传输层经验；但 downloadId 的网络生命周期不能替代 Profile 数据事务。

## 3. 拆清五类状态

### 3.1 SourceCatalog：平台事实

保存 `sourceId + remoteGameKey + remoteModId` 的展示元数据、版本清单及 `latestAvailableVersion`，带 catalogRevision/来源时间。

远端 originalName 与用户 alias 分开；刷新平台名称不覆盖用户编辑的显示名。不能把“最新可用版本”直接当“已下载版本”。

### 3.2 ProfileContent：用户意图

每条 ProfileMod 保存：

- profileId、稳定 profileModId、incarnationId；删除后重新添加是新 incarnation。
- ModRef、enabled、folder/order。
- `versionSelection = auto | pinned(versionId)`。
- 当前已接受的 resolvedVersion/content artifact 绑定，或明确 not-ready 状态。
- contentRevision/entryRevision。

`auto` 表示后续显式更新操作可以解析最新版本，不表示收到任意旧回调时就自动覆盖用户当前配置。

显示偏好、选择中的游戏、窗口状态不递增 contentRevision。用户 alias 使用独立 entityRevision；主文档的 expectedRevision 涉及列表内容时落实为 expectedContentRevision。

### 3.3 ContentArtifact：已经存在且校验过的字节

保存 digest、size、格式、源文件版本身份和验证级别。缓存路径是 digest 的实现细节，不是由 Mod 名称唯一定位的业务状态。

同一 Mod 的 v1、v2 可以同时缓存。两个 Profile 引用不同版本是正常状态，不应争夺一条 `mod_downloads(modId)` 记录。

### 3.4 Transfer / DownloadOperation：进行中的工作

- Transfer：某次具体字节传输，含 transferId、attemptId、来源文件身份、checkpoint、seq 和网络状态。
- DownloadOperation：用户用例，含目标 Profile、固定输入、所需 transfers、原子应用策略和能力锁。
- Consumer：哪个 operation/job 需要哪个 transfer/artifact。

一次 transfer 可被多个 consumer 使用；取消一个用户操作不自动取消其他 consumer 的相同下载。

### 3.5 InstallReceipt：游戏里实际安装的状态

只有安装提交成功才能更新 installed 状态。下载完成或 Profile 选择变化不能把游戏标记为已更新。

## 4. 两种下载模式，不能隐式混用

### 4.1 `cache-only`：纯预下载

目的：取得一个已经明确的版本文件，缓存备用。

- 不改 Profile 的成员、顺序、启用状态或 versionSelection。
- 用户可以继续增删、切换版本和排序。
- 下载完成只增加已验证的内容工件；不会重新把已删除行插回列表。
- 若原调用者已不再需要该版本，它仍可作为缓存保留，按引用规则回收。

### 4.2 `prepare-and-apply`：下载并更新当前配置

目的：当前产品的批量更新、版本切换、需要下载后才能完成的添加操作。

- 固定要更新的 Profile 和计划。
- 获取该 Profile 的 `content.write` 排他逻辑锁。
- 网络期间不持有 SQLite transaction，只持有持久的操作租约。
- 所有文件完成校验后，在一个短事务中切换该操作涉及的绑定、版本选择和 contentRevision。
- 任一必需文件失败：Profile 保持旧可用状态；下载成功的文件可以留在缓存。
- 不能默认只应用成功的半批。用户明确选择部分应用时，生成新的操作与预期 revision。

版本切换 UI 可展示“准备切换到 v2”，但已生效版本仍为 v1；不能先把 v2 写成生效，再等网络失败后尝试拼凑回滚。

批量下载按钮若仅为了预热缓存，应命名为预下载；“更新当前配置”采用后者。客户端必须明确传 mode，后端不根据有没有打开 GUI 来猜。

## 5. 原子能力锁是什么

### 5.1 与权限的区别

- PermissionGrant：你是否被允许做这个操作。
- OperationLease：当前资源是否允许这个操作与其他操作并行。
- Database Transaction：这一小段状态变更是否原子提交。

三者都需要。拥有权限不代表资源没被占用；持有 lease 不代表可以跳过最终条件检查；数据库事务不能跨几分钟网络请求保持开启。

### 5.2 权威位置

Profile 内容及其逻辑 lease 由 Library 单写者管理，相关表位于同一个 `library.sqlite`。Gate 的检查和目标写入在同一个数据库事务内进行。

内核提供通用 OperationGate 库、任务监管、工件租约和能力查询路由，但不把 Library 的锁独立存到另一个数据库，再用 RPC “先查锁、后写库”。那会重新产生检查与使用之间的竞态。

不同数据所有者各自保证其本地原子性。跨 owner 流程通过持久 operation/outbox 协调，不声称具有一个跨数据库的原子锁。

### 5.3 资源和冲突矩阵

首轮只需要少数明确资源，不建设无约束的全局锁系统：

| 资源/能力 | 持有者 | 冲突对象 |
| --- | --- | --- |
| `profile:<id>:content.write` | 下载并应用、短内容编辑、导入/删除 Profile | 同 Profile 的所有内容变更 |
| `content:<digest>:read` | 构建/安装任务 | 内容回收；不阻止其他读取 |
| `transfer:<file-key>:produce` | 一个 transfer owner | 对同一确定文件的重复传输，可改为共享 consumer |
| `installation:<root-identity>:write` | 游戏安装/卸载 | 同物理目标的其他写事务 |
| `module-state:<id>:migrate` | 有状态模块迁移 | 该数据所有者的活跃写 lease/不兼容实例 |

每个 Profile 的门禁支持：无长租约时，普通编辑在短事务中检查空闲并更新；有长租约时，只有持有匹配 fence 的操作可以提交其预先声明的结果。

客户端没有通用“随便 acquire/release 锁”权限。它调用 start/cancel/update 等用例；用例处理器决定和申请资源。

### 5.4 下载期间允许什么

以下针对默认 `prepare-and-apply`：

| 用户操作 | 同一 Profile | 其他 Profile |
| --- | --- | --- |
| 查看、搜索、滚动、筛选、查看日志 | 允许 | 允许 |
| 修改显示名/备注等与构建无关元数据 | 独立字段事务允许 | 允许 |
| 切换界面当前游戏/Profile | 允许；不会改变下载目标 | 允许 |
| 新增/删除 Mod | 返回 RESOURCE_BUSY，不先乐观写入 | 允许 |
| 启用/禁用、切版本、排序、移动分组 | 返回 RESOURCE_BUSY | 允许 |
| 删除/导入覆盖 Profile | 返回 RESOURCE_BUSY | 允许，但不能全库恢复覆盖仍有任务的数据 |
| 清理被任务引用的缓存 | 拒绝回收这些工件 | 不影响无引用内容 |
| 取消当前操作后编辑 | 撤销提交权完成后允许 | 允许 |
| 另一次同目标更新 | 相同幂等键返回原操作；不同请求返回 busy | 可并行，遵守 transfer 共享规则 |
| 开始安装当前配置 | 默认等待更新结束，或用户明确选用旧已确认 snapshot | 独立目标可并行 |

不默认排队悄悄执行用户过时的点击。繁忙响应包含 ownerOperationId、原因、是否可取消；用户可明确重试。

## 6. 数据模型与 fencing

Library 的必要记录：

```text
profile_content_revisions
  profile_id PK
  content_revision
  lease_epoch

operation_leases
  resource_key PK
  operation_id
  fence_epoch
  owner_generation
  state
  last_heartbeat_at

download_operations
  operation_id PK
  mode
  profile_id
  expected_content_revision
  inputs_hash
  fence_epoch
  state
  result_ref
  idempotency_scope + idempotency_key UNIQUE

download_operation_items
  operation_id + item_id PK
  profile_mod_id
  incarnation_id
  source_id + remote_game_key + remote_mod_id
  resolved_version_id + remote_file_id
  expected_digest / source_validator
  artifact_digest
  item_state

outbox
  event_id PK
  operation_id
  type
  payload
  delivery_state
```

数据类型与主契约同步生成，以上是字段要求，不是一份省略约束的可直接执行迁移 SQL。

关键规则：

- fence_epoch 是数据库内单调递增的资源代际；不靠时间戳、PID 或随机 UI 锁值判断新旧。
- lease 撤销、恢复新尝试、转交所有权都会推进 fence。
- 旧 worker 即使还活着、旧 HTTP 回调晚到，也不能携带旧 fence 提交当前 Profile。
- 删除再添加相同 Mod 是新 incarnation。即使 modId 一样，旧回调也不能“复活”旧行。
- 幂等键相同且参数相同返回原 operation；相同键不同 inputsHash 返回冲突。
- 记录终态与结果，不在开始 confirm 后立刻删除唯一状态。

## 7. 原子开始、网络执行、原子提交

### 7.1 解析阶段

获取目标明确的 Profile snapshot，解析来源版本、依赖、权限和下载条件。需要浏览器授权时先完成授权，避免用户几分钟不操作就锁死列表。

解析期间列表可以变化。真正开始时必须带 expectedContentRevision 校验；不匹配就重新展示/解析，不偷偷套用到新列表。

### 7.2 开始事务：一个线性化点

同一 Library 数据库短写事务内完成：

```text
BEGIN IMMEDIATE
  查询幂等记录，已存在则返回原结果
  校验权限与明确目标身份
  校验 Profile contentRevision、各行 incarnation
  检查 profile content.write 无冲突 lease
  递增 lease_epoch，建立 operation lease
  插入 operation + 不可变 item 输入
  写 outbox(download.requested)
COMMIT
```

真正的“取得锁”和“记录我要下载并更新哪些内容”同时提交。两个并发请求不能都先看到 unlocked 后再各自开始。

不会在这一事务里调用网络、弹窗、长时间计算或等待其他 worker。

### 7.3 可靠启动任务

内核消费 Library outbox，以 operationId 作为持久幂等键登记 job。领取后宕机可以重发同一消息，不重复创建传输或丢失操作。

- Library 已提交、job 尚未创建：重放 outbox，操作保持 queued，可显式取消。
- job 已创建、确认丢失：重放返回同一个 jobId。
- 不能把两个数据库的两个独立写入当作天然原子提交。

### 7.4 下载和文件发布

- 每次 attempt 使用独立 `.part`/staging 路径。
- 输出不能覆盖仍由旧版本使用的 cachePath。
- 中断续传绑定服务端 validator；远端 ETag/文件身份变化时不能盲目拼接旧部分。
- 下载完成先校验长度、可用的来源摘要、格式和内容 SHA-256。
- 经验证的文件原子发布为不可变 content artifact，再产生 verified handle。
- 传输字节达到 100% 后仍可能处于 verifying/publishing，不能把它提前显示成“配置已更新”。

来源不提供可信摘要时，本地 SHA-256 只证明内容身份和本地完整性，不额外证明发布者身份。涉及 native DLL/脚本的用户风险批准仍然需要保留。

R14 落地位置为 `runtime/transfers`。持久表分别记录 transfer、attempt、consumer、artifact reference 和单调事件；broker 提供有界流式响应，下载 URL/凭据不写入 transfer 身份。进程恢复只把带 validator 的已同步 checkpoint 作为候选，后续仍使用来源重新解析出的新 descriptor；R15 将 verified artifact grant 移交给 Library operation 完成条件提交。

R15 落地位置为 `runtime/downloads` 与 Library schema v4。`mods.setVersion` 不再提前写 version selection；Library 在同一开始事务固定 source/game/Mod/version/file/selection，`cache-only` 使用 operation 级 lease，`prepare-and-apply` 使用 Profile-content lease。协调器逐项把已验证 artifact 记录为缓存，只有整批 grant 齐全才进入 job commit 和 Library 原子 apply；失败只终止 operation，不改旧 Profile binding。SourceProvider 的通用 begin/resume 接口把 Nexus 等交互继续绑定到相同 operation、plan hash、target revision 和 module-lock digest。

进程恢复以 Library operation 为提交事实：安全 job checkpoint 在新 generation 下取得新 fence 后继续；若 Library 已提交而 job 响应前中断，则直接用持久 result 完成 unknown commit，不重复推进 contentRevision。取消先在 Library 撤销提交权，再移除 transfer consumer；迟到网络结果无法越过终态/fence。Library 的 verified content refs 与 transfer durable refs 分别提供 GC 保留根。

### 7.5 应用事务：第二个线性化点

所有必需 item 就绪后，同一 Library 事务中：

```text
BEGIN IMMEDIATE
  若 operation 已终态，返回已记录结果
  校验 operation.state 与当前 operation lease
  校验 fence、owner generation、expectedContentRevision
  校验所有目标 incarnation 与输入版本身份
  校验所有 artifact handle 已验证且仍有引用保护
  仅更新该计划声明的版本选择/内容绑定
  原子推进 Profile contentRevision
  写 operation.succeeded + result_ref + outbox(profile.changed)
  释放本操作 lease；保留单调 fence 历史
COMMIT
```

若 artifact ownership 位于内核，verified handle 在此事务前取得持久 consumer 引用，不能先释放引用再提交绑定。绑定成功后通过 outbox 将临时引用转为长期引用；崩溃期间允许暂时多保留缓存，不允许过早 GC。

Library 不接受 worker 直接发送“把这个 CompleteModData 覆盖到数据库”。只接受属于本 operation 的 artifact 结果和窄字段应用命令。

失败条件：`STALE_OPERATION`、`STALE_FENCE`、`STALE_TARGET`、`REVISION_CONFLICT` 或 `ARTIFACT_NOT_VERIFIED`。均不改当前 Profile；已经验证的字节可留作缓存。

### 7.6 数据库与文件系统不是一个事务

顺序固定为：**验证并发布内容文件 -> 保持 artifact 引用 -> 提交业务绑定 -> outbox 传播**。

- 文件发布成功但 DB 尚未提交：最多多一份缓存，Profile 不会指向不存在的文件。
- DB 提交成功但响应丢失：按幂等记录找回原成功结果。
- outbox 未被消费：重放增量事件，不重新下载或重新应用。
- 对游戏目录的最终写入仍由独立文件 journal 保护，不与下载缓存事务混为一谈。

## 8. 同 Mod、多版本、多 Profile 与共享下载

不能只以 modId 去重。一个可靠文件身份至少包括 source、remoteGame、remoteMod、resolvedVersion、remoteFile 和影响字节的变体。

来源提供稳定不可变文件身份/摘要时，可以共享 transfer；短期签名 URL 只是访问凭据，不作为版本身份。来源无法提供可靠文件身份时，不做未经证明的下载前合并，取得字节后按 digest 去重。

示例：

```text
Profile A / Mod X / v1 -> operation A -> transfer T1 -> artifact H1
Profile B / Mod X / v2 -> operation B -> transfer T2 -> artifact H2
Profile C / Mod X / v1 -> operation C -> shared T1 -> artifact H1
```

A 的操作取消只移除 A 的 consumer；C 仍需要 T1，就继续下载。B 完成得更早或更晚，都不覆盖 A/C 的选择。全局源元数据记录 latestAvailableVersion=v2，也不把 H1 变成 v2。

缓存 GC 必须同时检查 Profile 引用、operation/job 引用、安装/打包输入引用和未完成引用转移；不能因某个列表删掉一行就删除其他消费者的文件。

## 9. 取消、超时、崩溃、热更新

### 9.1 用户想立即编辑被锁列表

明确执行“取消本次更新并解除占用”：

1. 在 Library 事务内将 operation 标为取消/撤销提交权，推进 fence。
2. 原子释放该 Profile lease，写取消 outbox。
3. 现在允许新内容编辑；下载 worker 后续只能完成/丢弃缓存，不能应用旧计划。
4. 内核移除该 operation 的 transfer consumers；没有其他 consumer 才取消实际网络请求。

不能先把前端 `isDownloading=false`，再等几秒钟通知后端取消。也不需要为了允许用户编辑而等待远端 HTTP 一定及时停止。

若应用事务已先成功提交，取消返回“已完成，未取消”，并给出新 revision；绝不能对用户声称改动被撤回。取消与提交在同一 Library owner 中线性化，谁先提交谁决定结果。

### 9.2 超时与租约

客户端超时只表示等待结束，不自动释放后端操作 lease。

心跳过期先将 owner 标记为可疑，由 supervisor 触发恢复/撤销流程；不能只按 TTL 删除锁记录。必须先推进 fence、阻止旧 worker 提交，再允许新的 owner。

安全性来自 fence 和条件提交，心跳仅服务于活性。时钟回拨、休眠、网络慢不能使两个 writer 同时拥有有效提交权。

### 9.3 进程重启

启动时先恢复 operation/lease/outbox，再对外公布可编辑能力：

- 确认旧 worker 已被 supervisor 收回或提交权失效。
- 下载可基于匹配的 validator/checkpoint 恢复；不能恢复则明确失败。
- 恢复操作需要新 attempt/fence，并重新检查目标 revision。
- 已终态 operation 的残留 lease 可在一个短事务中释放。
- 状态不确定时显示恢复占用，不能无依据把所有锁清空。

### 9.4 模块热更新

transfer/job 固定 provider 和运行时 digest，不能换到刚发布的新解析语义。

- 无状态下载 provider 可让旧实例完成，新的请求走新实例。
- Library schema 更新必须排空/维护，不能让旧 Library worker 在新 schema 上完成回调。
- 若选择暂停并释放 Profile lease，推进 fence，旧 operation 的自动应用权随之撤销；恢复时重新取得 revision 和授权。
- 缓存内容、Profile 能力锁、游戏目录锁与模块工件租约是不同对象；不能因“模块已更新”统一删除。

## 10. UI 状态与事件协议

权威列表与传输 overlay 分开：

```text
Library query cache
  keyed by profileId + entity identity + contentRevision

Transfer overlay
  keyed by operationId + transferId + attemptId

组合显示
  显示名、分组、启用、顺序来自 Library
  进度、速度、等待验证来自 Transfer
```

TransferProgress 只包含身份、seq、字节计数、阶段、速度等传输字段。它不能包含 displayName、enabled、folderId、order、usedVersion 等可编辑字段。

处理规则：

- seq 倒退、旧 attempt、旧 operation 的事件不覆盖新进度。
- 列表行已删除时，进度事件不创建该行。
- 业务提交发 `profile.changed(profileId,contentRevision,changedIds)`；query cache 按 revision 更新/失效。
- UI 切换 Profile 只换查询视图，不更改任务目标。
- 未挂载组件不影响任务数据；不要依赖模块级 `Map<modId,percent>` 作为权威状态。
- 能力查询返回 `allowed/reason/ownerOperationId/cancelAction/capabilityRevision`，所有菜单、快捷键、拖拽入口使用同一结果。
- 能力快照过期也不会造成数据错乱：后端每次 mutation 仍原子检查；UI 不先修改本地权威列表再碰运气。
- `RESOURCE_BUSY` 清楚显示“此配置正在更新 12 个 Mod”，提供查看任务和取消后编辑，而不是笼统禁止整个应用。

能力 availability 与静态权限 grants 分开返回，避免把“没有权限”与“暂时被任务占用”混在一个 disabled 布尔值里。

R16 落地为 `profiles.contentState` 与 vNext desktop Library panel。Library 在一个只读事务返回 contentRevision、lease owner/recovery 状态及活动 operation 固定项；daemon 再关联 durable job 和当前 active consumer 的 transferId/attemptId/seq/phase/字节数。UI 的 Library cache 只存 `mods.list` 行，overlay 只按现有 `profileModId` 追加 pending version/progress，operation 消失或 contentRevision 变化时重新查询 Library，不从进度载荷重建实体。

所有内容入口（单项/批量启停、版本、拖拽、菜单、快捷键移动和删除）读取同一 `writable` capability；Profile 切换、搜索、任务查看/取消与 shared rename 保持可用。renderer 初始等待 capability 时默认禁用内容写，后端 mutation 仍执行 R07 的原子 gate。轮询串行且 Profile generation 匹配后才应用，因此旧 Profile、旧 operation 或旧 transfer 结果不能覆盖当前行，也不能创建幽灵行。

## 11. 需要迁移的入口

必须一起迁移，不能只保护批量下载按钮：

1. `ModUpdateService.updateModFile/batchDownloadModFiles/updateModMetadataOnly` 的三种职责分离为 catalog refresh、transfer、apply operation。
2. `TreeViewItem` 版本选择不再提前写 usedVersion/currentVersion 或直接触发下载；调用同一 prepare-and-apply 命令。
3. `HomeService` 的添加、删除、启停、批量启停、移动、分组、版本切换和显示名编辑按字段 owner 接入门禁。
4. `ProfileDAO` 的更新/批量/导入路径迁到 Library transaction，不保留直接写入口绕过门禁。
5. `ModDAO` 的单 modId 下载/版本模型迁为多版本内容模型，旧字段只参与一次性迁移，不保留双写。
6. `OnlineModAddService`、搜索加入、剪贴板/deep link 加入、配置导入全部使用同一用例。
7. `ModInstallTask`、更新检查、Agent 工具、CLI 全部显式固定目标，不依赖 active Profile。
8. 删除全量 `mod-treeview-update` 在下载进度链上的数据覆盖，改为传输事件与领域提交事件。
9. 全局配置恢复、清缓存、删除游戏/Profile 等管理入口也遵循占用检查，不能成为后门。

原子门禁必须在 Library 接管写入时建立，不能留到游戏插件或 UI 框架拆完才补。没有门禁的旧直接写路径存在时，不开放新下载并应用功能给真实用户。

## 12. 必须通过的并发验收

以下是协议的权威验收要求。R07、R14、R15 的确定性测试覆盖后端线性化、传输和恢复行；R16 的模型、IPC 与真实浏览器 smoke 覆盖所有现有 UI 内容入口、允许入口及增量 overlay。

| 调度/故障 | 正确结果 |
| --- | --- |
| 下载开始与删除列表项同时请求 | 只有一个先取得线性化点：先下载则删除 busy；先删除则下载 stale target |
| 两个客户端同时请求同 Profile 不同更新 | 只接受一个 owner，另一个 busy；相同幂等请求返回同一 operation |
| 下载中拖拽、启停、切版本、导入覆盖 | 所有入口一致拒绝，不存在快捷键/CLI 绕过 |
| 下载中重命名/查看另一个 Profile | 允许，完成进度不覆盖名称或切回旧列表 |
| cache-only 下载时删除/重加原 Mod | 下载可完成到缓存，新 incarnation 不被旧回调修改 |
| A 下载 v1，B 下载 v2，结束顺序反转 | 两个 Profile 保持各自版本与正确摘要 |
| 下载 v1 期间平台发布 v2 | v1 completion 不把在线更新标记冒充为已获取 v2 |
| 批量第 N 个下载失败 | Profile 所有旧绑定保持，成功文件仅作为缓存 |
| 版本切换下载失败 | 生效版本和文件仍为原版本，pending 状态明确失败 |
| 取消与 apply 同时发生 | 按事务顺序得到取消或已成功，不能两者都声称成功 |
| 撤销 fence 后旧 worker 发送 success | 被拒绝应用，即使网络确实下载成功 |
| 心跳过期、旧 worker 尚未退出 | 新 owner 的提交权不会与旧 fence 重叠 |
| 文件发布后/DB commit 前崩溃 | 多余缓存可回收，Profile 无半提交 |
| DB commit 后/响应前崩溃 | 查询原 operation 返回一次成功，不重复修改 revision |
| outbox 重复消费 | 同一个 job/结果，无重复下载应用 |
| 两 consumer 共用 transfer，其中一个取消 | 另一个继续获得文件；已取消 Profile 不被更新 |
| Profile 更新时同时清缓存 | 引用中的字节不被删除 |
| UI reload、旧事件晚到、新任务已开始 | 新列表、新进度不回退，无幽灵条目 |
| Library/游戏/provider 模块更新 | 旧操作用原契约完成，或经明确暂停/撤销重新计划 |

测试采用可控 barrier/调度点、临时数据库与临时内容目录，断言业务状态和文件摘要。不能靠 sleep 碰运气，也不能只测试 `locked=true` 或按钮 disabled。

## 13. 这部分设计的完成标准

一次下载完成只能产生两类结果：

- **字节已验证并进入缓存**；
- **满足同一操作的全部前置条件，原子应用到指定 Profile**。

两者在 API、数据库、事件和 UI 中都必须可区分。原子能力锁约束允许发生的并发；不可变身份、fencing 和事务保证即使取消、重启或迟到回调发生，也不会把过去的结果写回现在的列表。
