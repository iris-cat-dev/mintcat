export type UiErrorContext = 'search' | 'bindings' | 'add' | 'host';
export interface UiErrorPresentation {
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
  if (/authorization|bearer|credential|api[-_ ]?key|access[-_ ]?token|password|secret|cookie/i.test(firstLine)) return '敏感诊断内容已隐藏';
  return firstLine.replace(/https?:\/\/[^\s<>"']+/gi, '[地址已隐藏]').slice(0, 600);
}
function identifier(value: string): string { return /^[\w./:-]{1,160}$/.test(value) ? value : '[标识已隐藏]'; }
export function presentUiError(cause: unknown, context: UiErrorContext = 'host'): UiErrorPresentation {
  const failure = payload(cause);
  const ordinary = typeof cause === 'string' ? cause : record(cause) && typeof cause.message === 'string' ? cause.message : '';
  // Some native control commands reject as CODE: message.key rather than AppError.
  const nativeCode = /^([A-Z][A-Z_]+):\s*[\w.]+$/.exec(ordinary)?.[1];
  const code = failure?.code ?? nativeCode ?? '';
  const reason = typeof failure?.details?.reason === 'string' ? failure.details.reason : '';
  const key = failure?.messageKey ?? '';
  let title = '操作未完成';
  let description = '请稍后重试；如果仍然失败，请展开技术详情并联系支持。';
  let retryable = failure?.retryable ?? true;
  if (code === 'SOURCE_UNAUTHORIZED' || reason === 'credential_missing_or_invalid') {
    title = '来源授权失败';
    description = '此来源缺少有效凭据，或凭据已失效。需要提供有效的来源凭据后才能访问；有其他已启用来源时，可手动切换浏览。';
    retryable = false;
  } else if (reason === 'source_configuration' || code === 'SOURCE_CONFIGURATION_INVALID' || (code === 'INVALID_ARGUMENT' && (context === 'search' || key.startsWith('source.')))) {
    title = '来源配置有误'; description = '此来源缺少必要设置，或不支持当前游戏。请切换其他来源；如需继续使用此来源，请联系维护者检查配置。'; retryable = false;
  } else if (code === 'SOURCE_RATE_LIMITED' || reason === 'source_rate_limited') {
    title = '来源请求过于频繁'; description = '来源暂时限制了请求，请等待一段时间后重试，或切换到其他已启用的来源。'; retryable = true;
  } else if (code === 'SOURCE_NOT_FOUND' || reason === 'source_not_found') {
    title = '来源中没有此资源'; description = '请确认游戏与来源绑定，或使用其他已启用的来源浏览。'; retryable = false;
  } else if (code === 'SOURCE_INTERACTION_REQUIRED' || reason === 'source_interaction_required') {
    title = '来源需要进一步确认'; description = '请完成来源要求的确认后再操作；有其他已启用来源时，也可手动切换浏览。'; retryable = false;
  } else if (code === 'SOURCE_INVALID_RESPONSE' || reason === 'source_response') {
    title = '来源数据无法读取'; description = '当前来源返回的数据无法识别或不完整。请稍后重试，或切换到其他已启用的来源。'; retryable = true;
  } else if (reason === 'source_network' || (!code && /network|fetch|connection|timeout|offline|ECONN/i.test(ordinary))) {
    title = context === 'host' ? '后台连接失败' : '暂时无法连接来源'; description = context === 'host' ? '请打开“后台状态与恢复”检查后台状态，再重试操作。' : '请检查网络连接，稍后重试；也可手动切换到其他已启用的来源。'; retryable = true;
  } else if (code === 'SOURCE_UNAVAILABLE' || reason === 'source_unavailable') {
    title = '来源暂时不可用'; description = '该来源当前无法完成请求。请稍后重试，或手动切换到其他已启用的来源。'; retryable = true;
  } else if (code === 'GAME_NOT_ENROLLED' || code === 'GAME_NOT_FOUND' || ((code === 'NOT_FOUND' || code === 'INVALID_ARGUMENT') && context === 'bindings')) {
    title = '游戏尚未注册或注册信息无效'; description = '请在游戏设置中注册真实安装目录，再到模组管理创建配置。来源浏览不需要添加配置。'; retryable = false;
  } else if (code === 'PROFILE_NOT_FOUND' || ((code === 'NOT_FOUND' || code === 'INVALID_ARGUMENT') && context === 'add')) {
    title = '目标配置不可用'; description = '请重新读取配置并选择有效的目标配置；没有配置时，请先在模组管理中创建。'; retryable = false;
  } else if (code === 'RESOURCE_BUSY' || code === 'CONFLICT' || code === 'REVISION_CONFLICT' || reason === 'resource_busy') {
    title = '配置正在处理或已发生变更'; description = '请等待当前任务完成，并重新读取配置后再操作。'; retryable = false;
  } else if (code === 'PERMISSION_DENIED' || code === 'SYSTEM_AUTHORIZATION_REQUIRED') {
    title = '当前操作未获授权'; description = '请确认当前模块和操作已获授权；不要重复提交尚未授权的操作。'; retryable = false;
  } else if (context === 'bindings') {
    title = '读取配置失败'; description = '请重试读取配置。仍可浏览来源；添加模组需要有效的游戏注册信息和目标配置。';
  } else if (context === 'search') {
    title = '来源浏览失败'; description = '请稍后重试，或手动切换到其他已启用的来源。';
  } else if (context === 'add') {
    title = '模组未能添加'; description = '请检查目标配置与当前任务状态后再操作。'; retryable = false;
  }
  const diagnostics: string[] = [];
  if (code) diagnostics.push(`错误码：${identifier(code)}`);
  if (failure?.requestId) diagnostics.push(`请求标识：${identifier(failure.requestId)}`);
  if (key) diagnostics.push(`消息标识：${identifier(key)}`);
  if (reason) diagnostics.push(`原因：${reason === 'credential_missing_or_invalid' ? '缺少有效来源凭据或凭据失效' : technicalText(reason)}`);
  const message = typeof failure?.details?.message === 'string' ? failure.details.message : ordinary;
  if (message && !payload(message)) diagnostics.push(`技术说明：${technicalText(message)}`);
  return { title, description, retryable, diagnostics };
}
export function renderUiError(target: HTMLElement, error: UiErrorPresentation): void {
  const title = document.createElement('strong'); title.textContent = error.title;
  const description = document.createElement('p'); description.textContent = error.description;
  target.replaceChildren(title, description);
  if (error.diagnostics.length) {
    const details = document.createElement('details');
    const summary = document.createElement('summary'); summary.textContent = '技术详情';
    const text = document.createElement('pre'); text.textContent = error.diagnostics.join('\n');
    details.append(summary, text); target.append(details);
  }
}
