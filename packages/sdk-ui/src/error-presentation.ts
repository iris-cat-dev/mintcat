import { tr } from './language';

export type UiErrorContext = 'search' | 'bindings' | 'add' | 'host';
export type UiErrorKind =
  | 'source-unauthorized'
  | 'source-credential-invalid'
  | 'source-configuration'
  | 'source-rate-limited'
  | 'source-not-found'
  | 'source-interaction-required'
  | 'source-invalid-response'
  | 'source-network'
  | 'source-unavailable'
  | 'cloud-prerequisite'
  | 'game-not-enrolled'
  | 'profile-not-found'
  | 'resource-busy'
  | 'permission-denied'
  | 'generic';
export interface UiErrorPresentation {
  kind: UiErrorKind;
  title: string;
  description: string;
  retryable: boolean;
  diagnostics: readonly string[];
}
interface ErrorPayload { code: string; messageKey: string; requestId?: string; details?: Record<string, unknown>; retryable?: boolean }
function record(value: unknown): value is Record<string, unknown> { return typeof value === 'object' && value !== null; }
function payload(value: unknown, depth = 0): ErrorPayload | undefined {
  if (depth > 3) return undefined;
  if (typeof value === 'string') {
    try { return payload(JSON.parse(value) as unknown, depth + 1); } catch { return undefined; }
  }
  if (!record(value)) return undefined;
  if (typeof value.code === 'string' && typeof value.messageKey === 'string') {
    return { code: value.code, messageKey: value.messageKey, requestId: typeof value.requestId === 'string' ? value.requestId : undefined,
      details: record(value.details) ? value.details : undefined, retryable: typeof value.retryable === 'boolean' ? value.retryable : undefined };
  }
  return payload(value.appError, depth + 1) ?? payload(value.error, depth + 1) ?? payload(value.message, depth + 1);
}
function technicalText(value: string): string {
  // Diagnostics are intentionally allowlisted, not a serialization of details or stack.
  const firstLine = value.split(/[\r\n]/, 1)[0];
  if (/authorization|bearer|credential|api[-_ ]?key|access[-_ ]?token|password|secret|cookie/i.test(firstLine)) return tr('敏感诊断内容已隐藏', 'Sensitive diagnostic text hidden');
  return firstLine.replace(/https?:\/\/[^\s<>"']+/gi, tr('[地址已隐藏]', '[URL hidden]')).slice(0, 600);
}
function identifier(value: string): string { return /^[\w./:-]{1,160}$/.test(value) ? value : tr('[标识已隐藏]', '[identifier hidden]'); }
const safeReasons: Readonly<Record<string, string>> = {
  credential_missing_or_invalid: tr('缺少有效来源凭据或凭据失效', 'Source credential missing or invalid'),
  credential_empty: tr('凭据内容为空', 'Credential is empty'),
  credential_invalid_format: tr('凭据包含空白或无效字符', 'Credential contains whitespace or invalid characters'),
  credential_vault_unavailable: tr('系统凭据库不可用', 'System credential store unavailable'),
  'source snapshot identity mismatch': tr('应用或游戏插件版本已更新，旧任务的快照版本不再匹配', 'The app or game plugin was updated and the old task snapshot no longer matches'),
};
export function presentUiError(cause: unknown, context: UiErrorContext = 'host'): UiErrorPresentation {
  const failure = payload(cause);
  const ordinary = typeof cause === 'string' ? cause : record(cause) && typeof cause.message === 'string' ? cause.message : '';
  // Some native control commands reject as CODE: message.key rather than AppError.
  const nativeCode = /^([A-Z][A-Z_]+):\s*[\w.]+$/.exec(ordinary)?.[1];
  const code = failure?.code ?? nativeCode ?? '';
  const reason = typeof failure?.details?.reason === 'string' ? failure.details.reason : '';
  const key = failure?.messageKey ?? '';
  let kind: UiErrorKind = 'generic';
  let title = tr('操作未完成', 'Could not complete the operation');
  let description = tr('请稍后重试；如果仍然失败，请展开技术详情并联系支持。', 'Try again later. If the problem persists, expand the technical details and contact support.');
  let retryable = failure?.retryable ?? true;
  if (code === 'RECOVERY_REQUIRED' || key === 'source.frozen_configuration_unavailable') {
    kind = 'generic';
    if (key === 'source.frozen_configuration_unavailable' || /snapshot identity mismatch/i.test(reason)) {
      title = tr('旧版未完成任务已失效', 'An unfinished task can no longer resume');
      description = tr('由于应用或游戏插件版本已更新，上次中断的后台任务无法自动继续。您可以直接移除这条记录，然后重新执行导入或下载操作。', 'The app or game plugin was updated, so the interrupted background task cannot resume automatically. Remove this entry, then start the import or download again.');
    } else {
      title = tr('后台任务已中断', 'Background task interrupted');
      description = tr('该任务在后台异常中断，无法自动续传。请移除这条记录后重新发起操作。', 'The task stopped unexpectedly and cannot resume automatically. Remove this entry, then start the operation again.');
    }
    retryable = false;
  } else if (code === 'CLOUD_PREREQUISITE') {
    kind = 'cloud-prerequisite';
    title = tr('云备份未配置', 'Cloud backup is not configured');
    description = tr('需要维护者配置 HTTPS 备份服务与下载域名；离线备份仍可使用。', 'A maintainer must configure an HTTPS backup service and download domain. Offline backups are still available.');
    retryable = false;
  } else if (code === 'SOURCE_UNAUTHORIZED' || reason === 'credential_missing_or_invalid') {
    kind = 'source-unauthorized';
    title = tr('来源授权失败', 'Source authorization failed');
    description = tr('此来源缺少有效凭据，或凭据已失效。需要提供有效的来源凭据后才能访问；有其他已启用来源时，可手动切换浏览。', 'This source requires a valid credential. Update the credential, or switch to another enabled source.');
    retryable = false;
  } else if (reason === 'credential_empty' || reason === 'credential_invalid_format') {
    kind = 'source-credential-invalid';
    title = tr('凭据格式无效', 'Invalid credential format');
    description = reason === 'credential_empty'
      ? tr('请输入有效的凭据后再保存。', 'Enter a valid credential before saving.')
      : tr('凭据不能包含空格、换行或非 ASCII 字符，请仅粘贴官方生成的令牌或 API Key 本身。', 'Credentials cannot contain spaces, line breaks, or non-ASCII characters. Paste only the token or API key issued by the source.');
    retryable = false;
  } else if (reason === 'credential_vault_unavailable') {
    kind = 'source-unavailable';
    title = tr('无法访问系统凭据库', 'Cannot access the system credential store');
    description = tr('读取、保存或清除凭据时无法访问本机系统钥匙串，请确认钥匙串已解锁并允许访问后重试。', 'Unlock the system credential store and allow access, then try again.');
    retryable = true;
  } else if (reason === 'source_configuration' || code === 'SOURCE_CONFIGURATION_INVALID' || (code === 'INVALID_ARGUMENT' && (context === 'search' || key.startsWith('source.')))) {
    kind = 'source-configuration';
    title = tr('来源配置有误', 'Invalid source configuration');
    description = tr('此来源缺少必要设置，或不支持当前游戏。请切换其他来源；如需继续使用此来源，请联系维护者检查配置。', 'This source is missing required settings or does not support this game. Switch to another source, or ask a maintainer to check the configuration.');
    retryable = false;
  } else if (code === 'SOURCE_RATE_LIMITED' || reason === 'source_rate_limited') {
    kind = 'source-rate-limited';
    title = tr('来源请求过于频繁', 'Too many requests to the source');
    description = tr('来源暂时限制了请求，请等待一段时间后重试，或切换到其他已启用的来源。', 'This source is temporarily rate-limiting requests. Wait and try again, or switch to another enabled source.');
    retryable = true;
  } else if (code === 'SOURCE_NOT_FOUND' || reason === 'source_not_found') {
    kind = 'source-not-found';
    title = tr('来源中没有此资源', 'Resource not found in this source');
    description = tr('请确认游戏与来源绑定，或使用其他已启用的来源浏览。', 'Check that the game is linked to this source, or browse another enabled source.');
    retryable = false;
  } else if (code === 'SOURCE_INTERACTION_REQUIRED' || reason === 'source_interaction_required') {
    kind = 'source-interaction-required';
    title = tr('来源需要进一步确认', 'This source requires confirmation');
    description = tr('请完成来源要求的确认后再操作；有其他已启用来源时，也可手动切换浏览。', 'Complete the confirmation required by this source before continuing, or switch to another enabled source.');
    retryable = false;
  } else if (code === 'SOURCE_INVALID_RESPONSE' || reason === 'source_response') {
    kind = 'source-invalid-response';
    title = tr('来源数据无法读取', 'Could not read source data');
    description = tr('当前来源返回的数据无法识别或不完整。请稍后重试，或切换到其他已启用的来源。', 'The source returned unrecognized or incomplete data. Try again later, or switch to another enabled source.');
    retryable = true;
  } else if (reason === 'source_network' || (!code && /network|fetch|connection|timeout|offline|ECONN/i.test(ordinary))) {
    kind = 'source-network';
    title = context === 'host' ? tr('后台连接失败', 'Could not connect to the backend') : tr('暂时无法连接来源', 'Could not connect to the source');
    description = context === 'host'
      ? tr('请打开“后台状态与恢复”检查后台状态，再重试操作。', 'Open “Backend Status & Recovery” to check the backend, then try again.')
      : tr('请检查网络连接，稍后重试；也可手动切换到其他已启用的来源。', 'Check your network connection and try again later, or switch to another enabled source.');
    retryable = true;
  } else if (code === 'SOURCE_UNAVAILABLE' || reason === 'source_unavailable') {
    kind = 'source-unavailable';
    title = tr('来源暂时不可用', 'Source temporarily unavailable');
    description = tr('该来源当前无法完成请求。请稍后重试，或手动切换到其他已启用的来源。', 'This source could not complete the request. Try again later, or switch to another enabled source.');
    retryable = true;
  } else if (code === 'GAME_NOT_ENROLLED' || code === 'GAME_NOT_FOUND' || ((code === 'NOT_FOUND' || code === 'INVALID_ARGUMENT') && context === 'bindings')) {
    kind = 'game-not-enrolled';
    title = tr('游戏尚未注册或注册信息无效', 'Game registration is missing or invalid');
    description = tr('请在游戏设置中注册真实安装目录，再到模组管理创建配置。来源浏览不需要添加配置。', 'Register the game’s installation folder in Game Settings, then create a profile in the Mod Library. You don’t need a profile to browse sources.');
    retryable = false;
  } else if (code === 'PROFILE_NOT_FOUND' || ((code === 'NOT_FOUND' || code === 'INVALID_ARGUMENT') && context === 'add')) {
    kind = 'profile-not-found';
    title = tr('目标配置不可用', 'Target profile unavailable');
    description = tr('请重新读取配置并选择有效的目标配置；没有配置时，请先在模组管理中创建。', 'Reload the profiles and choose a valid target. If there are no profiles, create one in the Mod Library.');
    retryable = false;
  } else if (code === 'RESOURCE_BUSY' || code === 'CONFLICT' || code === 'REVISION_CONFLICT' || reason === 'resource_busy') {
    kind = 'resource-busy';
    title = tr('配置正在处理或已发生变更', 'Profile is busy or has changed');
    description = tr('请等待当前任务完成，并重新读取配置后再操作。', 'Wait for the current task to finish, then reload the profile before trying again.');
    retryable = false;
  } else if (code === 'PERMISSION_DENIED' || code === 'SYSTEM_AUTHORIZATION_REQUIRED') {
    kind = 'permission-denied';
    title = tr('当前操作未获授权', 'Operation not authorized');
    description = tr('请确认当前模块和操作已获授权；不要重复提交尚未授权的操作。', 'Check that the module and operation are authorized before trying again.');
    retryable = false;
  } else if (context === 'bindings') {
    title = tr('读取配置失败', 'Could not load profiles');
    description = tr('请重试读取配置。仍可浏览来源；添加模组需要有效的游戏注册信息和目标配置。', 'Try loading profiles again. You can still browse sources, but adding mods requires a registered game and a valid target profile.');
  } else if (context === 'search') {
    title = tr('来源浏览失败', 'Could not browse the source');
    description = tr('请稍后重试，或手动切换到其他已启用的来源。', 'Try again later, or switch to another enabled source.');
  } else if (context === 'add') {
    title = tr('模组未能添加', 'Could not add the mod');
    description = tr('请检查目标配置与当前任务状态后再操作。', 'Check the target profile and current tasks before trying again.');
    retryable = false;
  }
  const diagnostics: string[] = [];
  if (code) diagnostics.push(tr('错误码：', 'Error code: ') + identifier(code));
  if (failure?.requestId) diagnostics.push(tr('请求标识：', 'Request ID: ') + identifier(failure.requestId));
  if (key) diagnostics.push(tr('消息标识：', 'Message key: ') + identifier(key));
  if (reason) {
    const diagnostic = safeReasons[reason] ?? technicalText(reason);
    diagnostics.push(tr(`原因：${diagnostic}`, `Reason: ${diagnostic}`));
  }
  const message = typeof failure?.details?.message === 'string' ? failure.details.message : ordinary;
  if (message && !payload(message)) {
    const diagnostic = technicalText(message);
    diagnostics.push(tr(`技术说明：${diagnostic}`, `Technical message: ${diagnostic}`));
  }
  return { kind, title, description, retryable, diagnostics };
}
export function renderUiError(target: HTMLElement, error: UiErrorPresentation): void {
  target.classList.add('mc-error-box');
  const title = document.createElement('strong');
  title.className = 'mc-error-title';
  title.textContent = error.title;
  const description = document.createElement('p');
  description.className = 'mc-error-description';
  description.textContent = error.description;
  target.replaceChildren(title, description);
  if (error.diagnostics.length) {
    const details = document.createElement('details');
    details.className = 'mc-error-details';
    const summary = document.createElement('summary');
    summary.className = 'mc-error-summary';
    summary.textContent = tr('技术详情', 'Technical details');
    const text = document.createElement('pre');
    text.className = 'mc-error-pre';
    text.textContent = error.diagnostics.join('\n');
    details.append(summary, text);
    target.append(details);
  }
}
