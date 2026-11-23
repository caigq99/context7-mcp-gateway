import { CircleHelp, ExternalLink, KeyRound } from 'lucide-react';
import { useState } from 'react';
import type { GuideEndpoints } from './api';
import { CopyBlock, Resource, useResource } from './components';
import { useLanguage } from './i18n';
import type { Text } from './i18n';
import { Link } from './navigation';

const clients = {
  codex: 'Codex',
  claudeCode: 'Claude Code',
  claudeDesktop: 'Claude Desktop',
  vscode: 'VS Code',
  cursor: 'Cursor',
  windsurf: 'Windsurf / Devin Local',
  mcp: 'MCP · Streamable HTTP',
  rest: 'REST · curl',
} as const;

type ClientId = keyof typeof clients;
const restOperations = [
  ['search_v3', 'restSearch'],
  ['fetch_v3', 'restFetch'],
  ['library_search_v2', 'restLibrarySearch'],
  ['context_v2', 'restContext'],
] as const;
type RestOperation = (typeof restOperations)[number][0];
type ClientGuide = {
  config: string;
  file?: string;
  docs: string;
  note: Text;
  environment?: 'key' | 'desktop';
};

export function SetupPage() {
  const { t } = useLanguage();
  const endpoints = useResource<GuideEndpoints>('/api/user/setup-guides');
  const [client, setClient] = useState<ClientId>('cursor');
  const [restOperation, setRestOperation] = useState<RestOperation>('search_v3');
  const [copied, setCopied] = useState(false);
  const [probeStatus, setProbeStatus] = useState<string | null>(null);
  const [probing, setProbing] = useState(false);

  const runProbe = async () => {
    setProbing(true);
    setProbeStatus('正在向网关健康检查端点发起探测...');
    try {
      const res = await fetch('/health/live');
      if (res.ok) {
        setProbeStatus('✓ 探测成功: HTTP 200 OK (Edge Tokio 网关运行正常，SSE/MCP 协议已就绪)');
      } else {
        setProbeStatus(`⚠ 探测返回状态: HTTP ${res.status}`);
      }
    } catch (e) {
      setProbeStatus('⚠ 探测连接失败，请确认本地网关服务正在监听 8787 端口');
    } finally {
      setProbing(false);
    }
  };

  return <>
    <div className="grid-section-header">
      <div>
        <h2 className="grid-section-title">客户端快速接入向导</h2>
        <span className="grid-section-meta">支持 Streamable HTTP 协议与各类主流开发客户端</span>
      </div>
      <Link className="btn-swiss-action btn-swiss-secondary" href="/console/tokens">
        <KeyRound size={15} aria-hidden="true" />
        {t('manageTokens')}
      </Link>
    </div>

    <Resource source={endpoints}>{(paths) => {
      const guide = makeGuide(client, paths, restOperation);
      const copyCode = async () => {
        try {
          await navigator.clipboard.writeText(guide.config);
          setCopied(true);
          setTimeout(() => setCopied(false), 2000);
        } catch {}
      };

      return (
        <div className="workbench-layout">
          {/* 左栏：客户端规格块 */}
          <div className="client-spec-block">
            <div className="spec-tab-bar">
              {(Object.entries(clients) as [ClientId, string][]).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  className={`spec-tab${client === id ? ' active' : ''}`}
                  onClick={() => setClient(id)}
                >
                  {label}
                </button>
              ))}
            </div>

            <div className="spec-content-view">
              <div className="spec-file-banner">
                <span style={{ fontWeight: 600, color: 'var(--subtle)' }}>
                  {guide.file ?? clients[client]}
                </span>
                <button type="button" className="btn-swiss-action" onClick={copyCode}>
                  {copied ? '✓ 已复制配置' : '一键复制代码'}
                </button>
              </div>

              {client === 'rest' && (
                <div style={{ marginBottom: '12px' }}>
                  <label className="field" style={{ fontSize: '13px' }}>
                    {t('restOperation')}
                    <select
                      value={restOperation}
                      onChange={(event) => setRestOperation(event.currentTarget.value as RestOperation)}
                      style={{ marginTop: '4px' }}
                    >
                      {restOperations.map(([id, label]) => (
                        <option key={id} value={id}>{t(label)}</option>
                      ))}
                    </select>
                  </label>
                </div>
              )}

              <pre className="code-swiss-print">{guide.config}</pre>

              {guide.environment && (
                <div style={{ marginTop: '16px', borderTop: '1px solid var(--line)', paddingTop: '14px' }}>
                  <div style={{ fontSize: '12.5px', fontWeight: 600, color: 'var(--ink)', marginBottom: '8px' }}>
                    本地环境变量设置 (Bash / zsh)
                  </div>
                  <pre className="code-swiss-print" style={{ fontSize: '12px' }}>
                    {terminalEnvironment(paths.token_env, guide.environment)}
                  </pre>
                </div>
              )}

              <div style={{ marginTop: '16px', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
                <a className="text-link" href={guide.docs} rel="noopener noreferrer" target="_blank">
                  <ExternalLink size={14} aria-hidden="true" />
                  {t(client === 'claudeDesktop' ? 'bridgeDocs' : 'docs')} →
                </a>
                <span style={{ fontSize: '12px', color: 'var(--quiet)' }}>{t(guide.note)}</span>
              </div>
            </div>
          </div>

          {/* 右栏：说明与探测 */}
          <div>
            <div className="workbench-box bench-bg">
              <h3 style={{ fontSize: '14.5px', fontWeight: 800, fontFamily: 'var(--font-display)', marginBottom: '12px', color: 'var(--ink)' }}>
                配置接入说明
              </h3>
              <ol style={{ marginLeft: '18px', lineHeight: 1.8, color: 'var(--subtle)', fontSize: '13.5px' }}>
                <li>将左侧配置代码完整粘贴至对应客户端的配置文件中。</li>
                <li>确保本地或环境已配置 Token 环境变量 <code>{paths.token_env}</code>。</li>
                <li>启动客户端并在对话框调用 <code>resolve-library-id ("tokio")</code> 即可完成首次握手。</li>
              </ol>
            </div>

            <div className="workbench-box">
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '12px' }}>
                <span style={{ fontFamily: 'var(--font-display)', fontSize: '14px', fontWeight: 700, color: 'var(--ink)' }}>
                  实时端点连通探测
                </span>
                <button
                  type="button"
                  className="btn-swiss-action btn-swiss-secondary"
                  style={{ padding: '4px 10px', fontSize: '11px' }}
                  disabled={probing}
                  onClick={runProbe}
                >
                  {probing ? '探测中...' : '发起探测'}
                </button>
              </div>
              <div style={{ fontFamily: 'var(--font-mono)', fontSize: '12.5px', color: probeStatus ? 'var(--ink)' : 'var(--quiet)', background: 'var(--bench)', padding: '12px', borderRadius: '4px', border: '1px solid var(--line)' }}>
                {probeStatus ?? '状态: 就绪，点击右上方按钮发起连通性探测'}
              </div>
            </div>

            <div className="workbench-box">
              <h3 style={{ fontSize: '14px', fontWeight: 700, fontFamily: 'var(--font-display)', marginBottom: '12px', color: 'var(--ink)' }}>
                网关端点清单
              </h3>
              <CopyBlock text={paths.origin} label={t('gatewayOrigin')} />
              <CopyBlock text={paths.mcp_url} label={t('mcpEndpoint')} />
              <CopyBlock text={paths.rest_origin} label={t('restEndpoint')} />
            </div>

            <div className="workbench-box bench-bg">
              <h3 style={{ fontSize: '14px', fontWeight: 700, fontFamily: 'var(--font-display)', marginBottom: '10px', color: 'var(--ink)', display: 'flex', alignItems: 'center', gap: '6px' }}>
                <CircleHelp size={16} aria-hidden="true" />
                {t('troubleshoot')}
              </h3>
              <ul className="guidance-list" style={{ fontSize: '13px', color: 'var(--subtle)' }}>
                <li>{t('authTrouble')}</li>
                <li>{t('quotaTrouble')}</li>
                <li>{t('cloudTrouble')}</li>
              </ul>
            </div>
          </div>
        </div>
      );
    }}</Resource>
  </>;
}

function makeGuide(id: ClientId, endpoints: GuideEndpoints, restOperation: RestOperation): ClientGuide {
  const { mcp_url: url, rest_origin, token_env: variable } = endpoints;
  switch (id) {
    case 'codex':
      return { config: `[mcp_servers.context7_gateway]\nurl = ${JSON.stringify(url)}\nbearer_token_env_var = "${variable}"`,
        file: '~/.codex/config.toml', note: 'configureInClient', environment: 'key', docs: 'https://learn.chatgpt.com/docs/extend/mcp?surface=cli' };
    case 'claudeCode':
      return { config: jsonConfig({ mcpServers: { context7_gateway: { type: 'http', url, headers: { Authorization: `Bearer \${${variable}}` } } } }),
        file: '.mcp.json', note: 'configureInClient', environment: 'key', docs: 'https://code.claude.com/docs/en/mcp#environment-variable-expansion-in-mcp-json' };
    case 'claudeDesktop':
      return { config: jsonConfig({ mcpServers: { context7_gateway: {
        command: 'npx', args: ['-y', 'mcp-remote@0.16.7', url, '--transport', 'http-only', '--header', 'Authorization:${GATEWAY_AUTH_HEADER}'],
      } } }), file: 'claude_desktop_config.json', note: 'clientClaudeDesktopNote', environment: 'desktop', docs: 'https://github.com/punkpeye/mcp-remote#custom-headers' };
    case 'vscode':
      return { config: jsonConfig({
        inputs: [{ type: 'promptString', id: 'context7-gateway-key', description: 'Context7 Gateway API key', password: true }],
        servers: { context7_gateway: { type: 'http', url, headers: { Authorization: 'Bearer ${input:context7-gateway-key}' } } },
      }), file: '.vscode/mcp.json', note: 'clientVsCodeNote', docs: 'https://code.visualstudio.com/docs/agents/reference/mcp-configuration' };
    case 'cursor':
      return { config: jsonConfig({ mcpServers: { context7_gateway: { url, headers: { Authorization: `Bearer \${env:${variable}}` } } } }),
        file: '~/.cursor/mcp.json', note: 'configureInClient', environment: 'key', docs: 'https://cursor.com/docs/mcp' };
    case 'windsurf':
      return { config: jsonConfig({ mcpServers: { context7_gateway: { serverUrl: url, headers: { Authorization: `Bearer \${env:${variable}}` } } } }),
        file: '~/.codeium/windsurf/mcp_config.json', note: 'clientWindsurfNote', environment: 'key', docs: 'https://docs.devin.ai/desktop/devin-local/customizations' };
    case 'mcp':
      return { config: mcpProbe(endpoints),
        note: 'genericMcpNote', environment: 'key', docs: 'https://modelcontextprotocol.io/specification/2025-11-25/basic/transports' };
    case 'rest': {
      const base = rest_origin.replace(/\/$/, '');
      const snippets: Record<RestOperation, { endpoint: string; params: string[] }> = {
        search_v3: { endpoint: 'v3/search', params: ['query=How do I spawn a task in Tokio?', 'library=Tokio', 'type=json'] },
        fetch_v3: { endpoint: 'v3/fetch', params: ['url=https://docs.rs/tokio/latest/tokio/task/index.html', 'query=How do I spawn a task?', 'type=json'] },
        library_search_v2: { endpoint: 'v2/libs/search', params: ['libraryName=tokio', 'query=asynchronous runtime'] },
        context_v2: { endpoint: 'v2/context', params: ['libraryId=/tokio-rs/tokio', 'query=How do I spawn a task?', 'type=json'] },
      };
      const selected = snippets[restOperation];
      const parameters = selected.params.map((param) => `  --data-urlencode '${param}'`).join(' \\\n');
      return {
        config: `${authorizationPipe(variable)} |\n  curl --silent --show-error --fail-with-body --get '${base}/${selected.endpoint}' \\\n  --header @- \\\n${parameters}`,
        note: 'restNote', environment: 'key', docs: 'https://context7.com/docs/api-guide',
      };
    }
  }
}

function jsonConfig(value: unknown) {
  return JSON.stringify(value, null, 2);
}

function terminalEnvironment(variable: string, mode: NonNullable<ClientGuide['environment']>) {
  const read = `printf '%s' 'Gateway API key: '\nIFS= read -rs ${variable}\nprintf '\\n'`;
  return mode === 'desktop'
    ? `${read}\nexport GATEWAY_AUTH_HEADER="Bearer $${variable}"\nunset ${variable}\nopen -a "Claude"`
    : `${read}\nexport ${variable}`;
}

function authorizationPipe(variable: string) {
  return `printf 'Authorization: Bearer %s\\n' "$${variable}"`;
}

function mcpProbe({ mcp_url, token_env }: GuideEndpoints) {
  const body = { jsonrpc: '2.0', id: 1, method: 'initialize', params: {
    protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'gateway-check', version: '1.0.0' },
  } };
  return `${authorizationPipe(token_env)} |\n  curl --silent --show-error --include --no-buffer '${mcp_url}' \\\n  --header @- \\\n  --header 'Accept: application/json, text/event-stream' \\\n  --header 'Content-Type: application/json' \\\n  --data '${JSON.stringify(body)}'`;
}
