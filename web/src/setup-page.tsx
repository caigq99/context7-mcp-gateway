import { CircleHelp, ExternalLink, KeyRound } from 'lucide-react';
import { useState } from 'react';
import type { GuideEndpoints } from './api';
import { CopyBlock, PageHeading, Panel, Resource, useResource } from './components';
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
  const [client, setClient] = useState<ClientId>('codex');
  const [restOperation, setRestOperation] = useState<RestOperation>('search_v3');
  return <>
    <PageHeading title="setup" help="setupHelp" />
    <Resource source={endpoints}>{(paths) => {
      const guide = makeGuide(client, paths, restOperation);
      return <div className="setup-grid">
        <div className="page-stack">
          <Panel title={t('clientSelection')}>
            <label className="field sr-only" htmlFor="client-select">{t('clientSelection')}</label>
            <select id="client-select" value={client} onChange={(event) => setClient(event.currentTarget.value as ClientId)}>
              {(Object.entries(clients) as [ClientId, string][]).map(([id, label]) => <option key={id} value={id}>{label}</option>)}
            </select>
            <p className="field-help">{t('selectKeyHelp')}</p>
            <Link className="text-link" href="/console/tokens"><KeyRound size={15} aria-hidden="true" />{t('manageTokens')}</Link>
          </Panel>
          <Panel title={t('gatewayOrigin')}>
            <CopyBlock text={paths.origin} label={t('gatewayOrigin')} />
            <CopyBlock text={paths.mcp_url} label={t('mcpEndpoint')} />
            <CopyBlock text={paths.rest_origin} label={t('restEndpoint')} />
          </Panel>
        </div>
        <div className="page-stack">
          {guide.environment && <Panel title={t('setEnvironment')} hint={t('envTerminal')}>
            <CopyBlock label="Bash / zsh" text={terminalEnvironment(paths.token_env, guide.environment)} />
            {client === 'claudeDesktop' && <p className="field-help">{t('claudeDesktopEnvNote')}</p>}
          </Panel>}
          <Panel title={`${t('configure')} · ${clients[client]}`} hint={t(guide.note)}>
            <CopyBlock label={guide.file ?? clients[client]} text={guide.config} />
            <a className="text-link" href={guide.docs} rel="noopener noreferrer" target="_blank"><ExternalLink size={15} aria-hidden="true" />
              {t(client === 'claudeDesktop' ? 'bridgeDocs' : 'docs')}
            </a>
          </Panel>
          <Panel title={t('verify')}>
            {client === 'rest' ? <>
              <label className="field">{t('restOperation')}<select value={restOperation} onChange={(event) => setRestOperation(event.currentTarget.value as RestOperation)}>
                {restOperations.map(([id, label]) => <option key={id} value={id}>{t(label)}</option>)}
              </select></label>
              <p>{t('restNote')}</p>
            </> : <>
              <p>{t('verifyStandard')}</p>
              <div className="code-inline"><code>resolve-library-id</code> <code>query-docs</code></div>
              <p>{t('verifyExamples')}</p>
              {client === 'mcp' && <>
                <p>{t('mcpProbeNote')}</p>
                <CopyBlock label="MCP initialize · 2025-11-25" text={mcpProbe(paths)} />
              </>}
            </>}
          </Panel>
          <Panel title={<><CircleHelp size={17} aria-hidden="true" /> {t('troubleshoot')}</>}>
            <ul className="guidance-list"><li>{t('authTrouble')}</li><li>{t('quotaTrouble')}</li><li>{t('cloudTrouble')}</li></ul>
          </Panel>
        </div>
      </div>;
    }}</Resource>
  </>;
}

function makeGuide(id: ClientId, { mcp_url: url, rest_origin, token_env: variable }: GuideEndpoints, restOperation: RestOperation): ClientGuide {
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
      return { config: `POST ${url}\nAuthorization: Bearer <${variable}>\nContent-Type: application/json\nAccept: application/json, text/event-stream`,
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
