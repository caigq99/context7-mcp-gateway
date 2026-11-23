import { KeyRound, Plus, RefreshCw } from 'lucide-react';
import { useEffect, useState } from 'react';
import { api } from './api';
import type { GatewayToken } from './api';
import { Confirm, CopyBlock, Dialog, EmptyState, ErrorNotice, Resource, StatusChip, Time, useMutation, useResource } from './components';
import { useLanguage } from './i18n';
import { Link } from './navigation';

type TokenAction = { type: 'create' } | { type: 'rotate' | 'revoke' | 'details'; token: GatewayToken }
  | { type: 'reveal'; token: string };

type InitialToken = { token: string; name: string };

export function InitialTokenNotice() {
  const { t } = useLanguage();
  const [issued, setIssued] = useState<InitialToken | null>(null);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  useEffect(() => {
    let active = true;
    void api<InitialToken | undefined>('/api/user/initial-token')
      .then((token) => { if (active && token) setIssued(token); })
      .catch(() => {});
    return () => { active = false; };
  }, []);
  if (!issued) return null;
  const copy = async () => {
    setCopyError(false);
    try {
      await navigator.clipboard.writeText(issued.token);
      setCopied(true);
    } catch {
      setCopyError(true);
    }
  };
  const acknowledge = () => {
    void api<void>('/api/user/initial-token/acknowledge', { method: 'POST' })
      .finally(() => setIssued(null));
  };
  return <Dialog title={t('initialKeyTitle')} onClose={acknowledge}>
    <p>{t('initialKeyHelp')}</p>
    <div className="code-inline">
      <code>{maskToken(issued.token)}</code>
      <button type="button" className="button button-sm" onClick={() => void copy()}>{t(copied ? 'copied' : 'copy')}</button>
    </div>
    {copyError && <p className="quota-warning" role="alert">{t('copyFailed')}</p>}
    <div className="actions modal-actions">
      <button type="button" className="button button-primary" onClick={acknowledge}>{t('initialKeySaved')}</button>
    </div>
  </Dialog>;
}

export function TokenPage({ detailId }: { detailId?: string }) {
  const { t } = useLanguage();
  return (
    <>
      <div className="grid-section-header">
        <div>
          <h2 className="grid-section-title">网关密钥清单</h2>
          <span className="grid-section-meta">客户端连接 Context7 网关的唯一鉴权凭据</span>
        </div>
        {detailId && (
          <Link className="btn-swiss-action btn-swiss-secondary" href="/console/tokens">
            {t('backToTokens')}
          </Link>
        )}
      </div>
      <p style={{ color: 'var(--subtle)', marginBottom: '20px', fontSize: '13.5px', lineHeight: 1.6 }}>
        Gateway Token 是客户端访问本网关的唯一凭据。网关会自动映射下游请求至上游健康密钥池，同时维护原子配额与调用审计。
      </p>
      <TokenManager path="/api/user/tokens" detailId={detailId} selfService readOnly />
    </>
  );
}

export function TokenManager({ path, detailId, selfService = false, readOnly = false, disabled = false }: { path: string; detailId?: string; selfService?: boolean; readOnly?: boolean; disabled?: boolean }) {
  const { t } = useLanguage();
  const tokens = useResource<GatewayToken[]>(path);
  const [action, setAction] = useState<TokenAction | null>(null);
  const [copyingId, setCopyingId] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [copyError, setCopyError] = useState(false);
  function reveal(token: GatewayToken) {
    tokens.refresh();
    if (token.token) setAction({ type: 'reveal', token: token.token });
    else setAction(null);
  }
  function details(token: GatewayToken) {
    return selfService
      ? <Link className="btn-swiss-action btn-swiss-secondary" style={{ padding: '4px 10px', fontSize: '11px' }} href={`/console/tokens/${encodeURIComponent(token.id)}`}>{t('details')}</Link>
      : <button type="button" className="btn-swiss-action btn-swiss-secondary" style={{ padding: '4px 10px', fontSize: '11px' }} onClick={() => setAction({ type: 'details', token })}>{t('details')}</button>;
  }
  async function copy(token: GatewayToken) {
    setCopyError(false);
    setCopyingId(token.id);
    try {
      const result = await api<{ token: string }>(`${path}/${encodeURIComponent(token.id)}/secret`);
      await navigator.clipboard.writeText(result.token);
      setCopiedId(token.id);
    } catch {
      setCopyError(true);
    } finally {
      setCopyingId(null);
    }
  }
  return (
    <>
      {copyError && <p className="quota-warning" role="alert">{t('copyFailed')}</p>}
      <div className="toolbar" style={{ marginBottom: '16px' }}>
        <div className="muted">{t('tokenHint')}</div>
        <div className="actions">
          <button type="button" className="button button-quiet icon-button" onClick={tokens.refresh} aria-label={t('refresh')}>
            <RefreshCw size={16} aria-hidden="true" />
          </button>
          {!readOnly && (
            <button type="button" className="btn-swiss-action" disabled={disabled} onClick={() => setAction({ type: 'create' })}>
              <Plus size={16} aria-hidden="true" />
              {t('createKey')}
            </button>
          )}
        </div>
      </div>
      <Resource source={tokens}>{(rows) => rows.length === 0
        ? <EmptyState title="tokenEmpty" hint={readOnly ? 'tokenReadOnlyEmptyHelp' : 'tokenEmptyHelp'} icon={<KeyRound size={22} />} />
        : <div className="bench-table-wrap">
            <table className="bench-table">
              <thead><tr>
                <th>密钥名称与标识</th>
                <th>授权范围</th>
                <th>创建时间</th>
                <th>最后活跃</th>
                <th>状态</th>
                <th>操作</th>
              </tr></thead>
              <tbody>{rows.map((token) => {
                const status = token.expires_at && new Date(token.expires_at).getTime() <= Date.now() ? 'expired' : token.status ?? 'active';
                const isGood = status === 'active';
                return <tr key={token.id}>
                  <td>
                    <strong style={{ fontFamily: 'var(--font-display)', color: 'var(--ink)' }}>{token.name}</strong>
                    <br />
                    <code style={{ color: 'var(--brand)', fontFamily: 'var(--font-mono)' }}>{maskPrefix(token.prefix)}</code>
                  </td>
                  <td><code>{token.scopes?.join(' / ') || 'mcp:all'}</code></td>
                  <td><Time value={token.created_at} /></td>
                  <td>{token.last_used_at ? <Time value={token.last_used_at} /> : <span style={{ color: 'var(--quiet)' }}>从未调用</span>}</td>
                  <td>
                    <span className="key-badge-status" style={{ color: isGood ? 'var(--good)' : 'var(--bad)' }}>
                      <span className="status-dot-emerald" style={{ background: isGood ? 'var(--good)' : 'var(--bad)' }}></span>
                      {isGood ? '正常使用' : status}
                    </span>
                  </td>
                  <td><div className="actions row-actions" style={{ gap: '6px' }}>
                    {details(token)}
                    {readOnly && (
                      <button
                        type="button"
                        className="btn-swiss-action btn-swiss-secondary"
                        style={{ padding: '4px 10px', fontSize: '11px' }}
                        disabled={!token.can_copy || copyingId === token.id}
                        onClick={() => void copy(token)}
                      >
                        {t(copiedId === token.id ? 'copied' : 'copy')}
                      </button>
                    )}
                    {!readOnly && status !== 'revoked' && status !== 'expired' && <>
                      {selfService && (
                        <button
                          type="button"
                          className="btn-swiss-action btn-swiss-secondary"
                          style={{ padding: '4px 10px', fontSize: '11px' }}
                          onClick={() => setAction({ type: 'rotate', token })}
                        >
                          {t('rotate')}
                        </button>
                      )}
                      <button
                        type="button"
                        className="btn-swiss-action btn-swiss-secondary"
                        style={{ padding: '4px 10px', fontSize: '11px', color: 'var(--bad)' }}
                        onClick={() => setAction({ type: 'revoke', token })}
                      >
                        {t('revoke')}
                      </button>
                    </>}
                  </div></td>
                </tr>
              })}</tbody>
            </table>
          </div>}
      </Resource>
      {detailId && !action && tokens.value.state === 'ready' && (
        <TokenDetails token={tokens.value.data.find((item) => item.id === detailId)} />
      )}
      {action?.type === 'details' && <TokenDetails token={action.token} onClose={() => setAction(null)} />}
      {action?.type === 'create' && <NewToken path={path} onClose={() => setAction(null)} onIssued={reveal} />}
      {action?.type === 'reveal' && <Dialog title={t('onceTitle')} onClose={() => setAction(null)}>
        <p>{t('onceHelp')}</p>
        <CopyBlock text={action.token} label={t('key')} />
        <div className="actions modal-actions">
          <button type="button" className="button button-primary" onClick={() => setAction(null)}>{t('close')}</button>
        </div>
      </Dialog>}
      {action?.type === 'rotate' && <RotateToken path={`${path}/${encodeURIComponent(action.token.id)}/rotate`}
        onClose={() => setAction(null)} onIssued={reveal} />}
      {action?.type === 'revoke' && <Confirm title="revokeTitle" help="revokeHelp" yes="revoke"
        onClose={() => setAction(null)} onConfirm={async () => {
          await api<void>(`${path}/${encodeURIComponent(action.token.id)}`, { method: 'DELETE' });
          tokens.refresh();
        }} />}
    </>
  );
}

function TokenDetails({ token, onClose }: { token?: GatewayToken; onClose?: () => void }) {
  const { t } = useLanguage();
  const standalone = !onClose;
  const content = token ? <>
    <dl className="definition-list">
      <div><dt>{t('keyName')}</dt><dd>{token.name}</dd></div>
      <div><dt>{t('keyPrefix')}</dt><dd><code>{token.prefix}</code></dd></div>
      <div><dt>{t('tokenId')}</dt><dd><code>{token.id}</code></dd></div>
      <div><dt>{t('status')}</dt><dd><StatusChip status={token.status ?? 'active'} /></dd></div>
      <div><dt>{t('created')}</dt><dd><Time value={token.created_at} /></dd></div>
      <div><dt>{t('lastUsed')}</dt><dd><Time value={token.last_used_at} /></dd></div>
      <div><dt>{t('expiration')}</dt><dd>{token.expires_at ? <Time value={token.expires_at} /> : t('none')}</dd></div>
      {token.scopes && <div><dt>{t('tokenScopes')}</dt><dd>{token.scopes.map((scope) => <code className="inline-code" key={scope}>{scope}</code>)}</dd></div>}
    </dl>
    {standalone && <div className="actions">
      <Link className="button" href="/console/tokens">{t('close')}</Link>
      <Link className="button button-primary" href={`/console/requests?token_id=${encodeURIComponent(token.id)}`}>{t('tokenRequests')}</Link>
    </div>}
  </> : <EmptyState title="notFound" />;
  return standalone
    ? <section className="subpanel" aria-label={t('tokenDetails')}><h3>{t('tokenDetails')}</h3>{content}</section>
    : <Dialog title={t('tokenDetails')} onClose={onClose}>{content}</Dialog>;
}

function NewToken({ path, onClose, onIssued }: { path: string; onClose: () => void; onIssued: (token: GatewayToken) => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  const [scopeError, setScopeError] = useState(false);
  return (
    <Dialog title={t('createKey')} onClose={() => { if (!mutation.busy) onClose(); }}>
      <form onSubmit={(event) => {
        event.preventDefault();
        const fields = new FormData(event.currentTarget);
        const name = String(fields.get('name')).trim();
        const expires = String(fields.get('expires_in_days') ?? '');
        const scopes = fields.getAll('scope');
        if (!name) return;
        if (scopes.length === 0) {
          setScopeError(true);
          return;
        }
        setScopeError(false);
        void mutation.run(async () => {
          const issued = await api<GatewayToken>(path, { method: 'POST', body: {
            name, scopes, expires_in_days: expires ? Number(expires) : undefined,
          } });
          onIssued(issued);
        });
      }}>
        <label className="field">{t('keyName')}<input name="name" required maxLength={120} autoFocus /></label>
        <label className="field">{t('expiresInDays')}<input type="number" name="expires_in_days" min="1" max="365" step="1" placeholder={t('none')} /></label>
        <fieldset className="weights">
          <legend>{t('tokenScopes')}</legend>
          <label className="checkbox-field"><input type="checkbox" name="scope" value="mcp" defaultChecked onChange={() => setScopeError(false)} />MCP</label>
          <label className="checkbox-field"><input type="checkbox" name="scope" value="context7:read" defaultChecked onChange={() => setScopeError(false)} />REST · Context7</label>
          {scopeError && <p className="quota-warning" role="alert">{t('tokenScopeRequired')}</p>}
        </fieldset>
        <ErrorNotice error={mutation.error} />
        <div className="actions modal-actions">
          <button type="button" className="button" onClick={onClose} disabled={mutation.busy}>{t('cancel')}</button>
          <button type="submit" className="button button-primary" disabled={mutation.busy}>{t(mutation.busy ? 'loading' : 'create')}</button>
        </div>
      </form>
    </Dialog>
  );
}

function RotateToken({ path, onClose, onIssued }: { path: string; onClose: () => void; onIssued: (token: GatewayToken) => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return (
    <Dialog title={t('rotateTitle')} onClose={() => { if (!mutation.busy) onClose(); }}>
      <p>{t('rotateHelp')}</p>
      <ErrorNotice error={mutation.error} />
      <div className="actions modal-actions">
        <button type="button" className="button" onClick={onClose} disabled={mutation.busy}>{t('cancel')}</button>
        <button type="button" className="button button-danger" disabled={mutation.busy} onClick={() => void mutation.run(async () => {
          const issued = await api<GatewayToken>(path, { method: 'POST' });
          onIssued(issued);
        })}>{t(mutation.busy ? 'loading' : 'rotate')}</button>
      </div>
    </Dialog>
  );
}

function maskToken(token: string) {
  const prefix = token.split('.', 1)[0];
  return `${prefix}••••••••`;
}

function maskPrefix(prefix: string) {
  return `${prefix}••••••••`;
}
