import { ArrowLeft, Database, KeyRound, Megaphone, Plus, RefreshCw, Search, Upload, UsersRound } from 'lucide-react';
import { useState } from 'react';
import { api, operations, quotaLimits } from './api';
import type { AdminUser, Announcement, BulkKeyImportResponse, BulkKeyImportStatus, KeyUsage, LedgerEntry, Order, Pool, QuotaAssignment, QuotaLimits, Settings, UpstreamKey } from './api';
import { PasskeySettings } from './auth';
import { Confirm, CopyBlock, Dialog, EmptyState, ErrorNotice, PageHeading, Panel, Resource, StatusChip, Time, useMutation, useResource } from './components';
import { useLanguage } from './i18n';
import type { Text } from './i18n';
import { Link, useNavigation } from './navigation';
import { TokenManager } from './token-page';
import { QuotaOverview } from './usage-pages';
import { price } from './billing-page';

const poolPath = '/api/admin/pools';
const usersPath = '/api/admin/users';
const bulkImportLimit = 100;
const bulkStatusLabel: Record<BulkKeyImportStatus, Text> = {
  created: 'bulkImportCreated',
  duplicate: 'bulkImportDuplicates',
  invalid: 'bulkImportInvalid',
};

export function PoolPage({ poolId }: { poolId?: string }) {
  const { t } = useLanguage();
  const { navigate } = useNavigation();
  const pools = useResource<Pool[]>(poolPath);
  const [creating, setCreating] = useState(false);
  return <>
    <PageHeading title="pools" help="poolDescription" actions={poolId
      ? <Link className="button" href="/admin/pools"><ArrowLeft size={16} aria-hidden="true" />{t('backToPools')}</Link>
      : <button className="button button-primary" type="button" onClick={() => setCreating(true)}><Plus size={17} aria-hidden="true" />{t('createPool')}</button>}
    />
    <Resource source={pools}>{(items) => {
      if (poolId) {
        const pool = items.find((item) => item.id === poolId);
        return pool ? <PoolDetails key={poolId} pool={pool} refresh={pools.refresh} /> : <Panel><EmptyState title="notFound" /></Panel>;
      }
      return <Panel title={t('pools')} actions={<button className="button button-quiet icon-button" type="button" aria-label={t('refresh')} onClick={pools.refresh}>
        <RefreshCw size={16} aria-hidden="true" />
      </button>}>
        {items.length === 0 ? <EmptyState title="poolEmpty" hint="poolEmptyHelp" icon={<Database size={22} />} /> :
          <div className="table-scroll"><table className="data-table">
            <thead><tr><th scope="col">{t('poolName')}</th><th scope="col">{t('scope')}</th><th scope="col">{t('status')}</th><th scope="col">{t('created')}</th><th scope="col"><span className="sr-only">{t('details')}</span></th></tr></thead>
            <tbody>{items.map((pool) => <tr key={pool.id}>
              <td><strong>{pool.name}</strong></td><td>{pool.authorization_scope}</td>
              <td><StatusChip status={pool.enabled ? 'enabled' : 'disabled'} /></td><td><Time value={pool.created_at} /></td>
              <td><Link className="button button-sm" href={`/admin/pools/${encodeURIComponent(pool.id)}`}>{t('managePool')}</Link></td>
            </tr>)}</tbody>
          </table></div>}
      </Panel>;
    }}</Resource>
    {creating && <NewPool onClose={() => setCreating(false)} onSuccess={(pool) => {
      pools.refresh();
      setCreating(false);
      navigate(`/admin/pools/${encodeURIComponent(pool.id)}`);
    }} />}
  </>;
}

function NewPool({ onClose, onSuccess }: { onClose: () => void; onSuccess: (pool: Pool) => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return <Dialog title={t('createPool')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <p>{t('poolNameHelp')}</p>
    <form onSubmit={(event) => {
      event.preventDefault();
      const fields = new FormData(event.currentTarget);
      const name = formText(fields, 'name');
      const authorization_scope = formText(fields, 'scope');
      if (name && authorization_scope) void mutation.run(async () => {
        onSuccess(await api<Pool>(poolPath, { method: 'POST', body: { name, authorization_scope } }));
      });
    }}>
      <label className="field">{t('poolName')}<input name="name" required maxLength={120} autoFocus /></label>
      <label className="field">{t('scope')}<input name="scope" required maxLength={120} /></label>
      <ErrorNotice error={mutation.error} />
      <FormActions busy={mutation.busy} submit="create" onClose={onClose} />
    </form>
  </Dialog>;
}

type KeyAction = { type: 'disable' | 'delete' | 'usage'; key: UpstreamKey };

function PoolDetails({ pool, refresh }: { pool: Pool; refresh: () => void }) {
  const { t } = useLanguage();
  const path = `${poolPath}/${encodeURIComponent(pool.id)}`;
  const keys = useResource<UpstreamKey[]>(`${path}/keys`);
  const mutation = useMutation();
  const [poolConfirm, setPoolConfirm] = useState(false);
  const [adding, setAdding] = useState(false);
  const [bulkImporting, setBulkImporting] = useState(false);
  const [action, setAction] = useState<KeyAction | null>(null);
  const [validation, setValidation] = useState<{ status: string; upstream_status: number } | null>(null);
  function keyPath(key: UpstreamKey) {
    return `${path}/keys/${encodeURIComponent(key.id)}`;
  }
  async function toggle(enabled: boolean) {
    await api<void>(`${path}/status`, { method: 'PATCH', body: { enabled } });
    refresh();
  }
  return <>
    <Panel title={pool.name} hint={pool.authorization_scope} actions={<>
      <StatusChip status={pool.enabled ? 'enabled' : 'disabled'} />
      <button className={`button${pool.enabled ? ' button-text-danger' : ''}`} type="button" disabled={mutation.busy} onClick={() => {
        if (pool.enabled) setPoolConfirm(true);
        else void mutation.run(() => toggle(true));
      }}>{t(pool.enabled ? 'disable' : 'enable')}</button>
    </>}>
      <dl className="definition-list">
        <div><dt>{t('pool')}</dt><dd><code>{pool.id}</code></dd></div>
        <div><dt>{t('created')}</dt><dd><Time value={pool.created_at} /></dd></div>
      </dl>
    </Panel>
    <Panel title={t('poolKeys')} actions={<>
      <button className="button button-quiet icon-button" type="button" aria-label={t('refresh')} onClick={keys.refresh}><RefreshCw size={16} aria-hidden="true" /></button>
      <button className="button" type="button" onClick={() => setBulkImporting(true)}><Upload size={16} aria-hidden="true" />{t('bulkImport')}</button>
      <button className="button button-primary" type="button" onClick={() => setAdding(true)}><Plus size={16} aria-hidden="true" />{t('addUpstreamKey')}</button>
    </>}>
      <ErrorNotice error={mutation.error} />
      {validation && <div className="notice" role="status"><span>{t('validationComplete')}</span><StatusChip status={validation.status} /><code>HTTP {validation.upstream_status}</code></div>}
      <Resource source={keys}>{(items) => items.length === 0 ? <EmptyState title="poolKeyEmpty" hint="poolKeyEmptyHelp" icon={<KeyRound size={22} />} /> :
        <div className="table-scroll"><table className="data-table">
          <thead><tr><th scope="col">{t('keyPrefix')}</th><th scope="col">{t('status')}</th><th scope="col">{t('lastChecked')}</th><th scope="col">{t('lastUsed')}</th><th scope="col"><span className="sr-only">{t('details')}</span></th></tr></thead>
          <tbody>{items.map((key) => <tr key={key.id}>
            <td><code>{key.display_prefix}</code></td><td><StatusChip status={key.status} /></td>
            <td><Time value={key.last_checked_at} /></td><td><Time value={key.last_used_at} /></td>
            <td><div className="actions row-actions">
              <button type="button" className="button button-sm" disabled={mutation.busy || key.status === 'disabled'} onClick={() => {
                setValidation(null);
                void mutation.run(async () => {
                  const result = await api<{ status: string; upstream_status: number }>(`${keyPath(key)}/validate`, { method: 'POST' });
                  setValidation(result);
                  keys.refresh();
                });
              }}>{t('validate')}</button>
              <button type="button" className="button button-sm" onClick={() => setAction({ type: 'usage', key })}>{t('upstreamUsage')}</button>
              {key.status === 'disabled'
                ? <button type="button" className="button button-sm" disabled={mutation.busy} onClick={() => void mutation.run(async () => {
                  await api<void>(`${keyPath(key)}/status`, { method: 'PATCH', body: { status: 'active' } });
                  setValidation(null);
                  keys.refresh();
                })}>{t('enable')}</button>
                : <button type="button" className="button button-sm button-text-danger" onClick={() => setAction({ type: 'disable', key })}>{t('disable')}</button>}
              <button type="button" className="button button-sm button-text-danger" onClick={() => setAction({ type: 'delete', key })}>{t('remove')}</button>
            </div></td>
          </tr>)}</tbody>
        </table></div>}
      </Resource>
    </Panel>
    {poolConfirm && <Confirm title="disablePoolTitle" help="disablePoolHelp" yes="disable" onClose={() => setPoolConfirm(false)} onConfirm={() => toggle(false)} />}
    {adding && <NewUpstreamKey path={`${path}/keys`} onClose={() => setAdding(false)} onSuccess={() => { setAdding(false); setValidation(null); keys.refresh(); }} />}
    {bulkImporting && <BulkImportUpstreamKeys path={`${path}/keys/bulk`} onClose={() => setBulkImporting(false)} onSuccess={() => { setValidation(null); keys.refresh(); }} />}
    {action?.type === 'usage' && <KeyUsageDialog path={`${keyPath(action.key)}/usage`} onClose={() => setAction(null)} />}
    {action?.type === 'disable' && <Confirm title="disableKeyTitle" help="disableKeyHelp" yes="disable" onClose={() => setAction(null)} onConfirm={async () => {
      await api<void>(`${keyPath(action.key)}/status`, { method: 'PATCH', body: { status: 'disabled' } });
      setValidation(null);
      keys.refresh();
    }} />}
    {action?.type === 'delete' && <Confirm title="removeKeyTitle" help="removeKeyHelp" yes="remove" onClose={() => setAction(null)} onConfirm={async () => {
      await api<void>(keyPath(action.key), { method: 'DELETE' });
      setValidation(null);
      keys.refresh();
    }} />}
  </>;
}

function NewUpstreamKey({ path, onClose, onSuccess }: { path: string; onClose: () => void; onSuccess: () => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return <Dialog title={t('addUpstreamKey')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <p>{t('upstreamKeyInputHelp')}</p>
    <form onSubmit={(event) => {
      event.preventDefault();
      const input = event.currentTarget.elements.namedItem('api_key');
      if (!(input instanceof HTMLInputElement)) return;
      const api_key = input.value.trim();
      input.value = '';
      if (api_key) void mutation.run(async () => {
        await api(path, { method: 'POST', body: { api_key } });
        onSuccess();
      });
    }}>
      <label className="field">{t('upstreamKeyInput')}<input type="password" required name="api_key" maxLength={1024} autoComplete="off" spellCheck={false} autoFocus /></label>
      <ErrorNotice error={mutation.error} />
      <FormActions busy={mutation.busy} submit="create" onClose={onClose} />
    </form>
  </Dialog>;
}

function BulkImportUpstreamKeys({ path, onClose, onSuccess }: { path: string; onClose: () => void; onSuccess: () => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  const [input, setInput] = useState('');
  const [clientError, setClientError] = useState<Text | null>(null);
  const [summary, setSummary] = useState<BulkKeyImportResponse | null>(null);
  return <Dialog title={t('bulkImport')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <p>{t('bulkImportHelp')}</p>
    <form onSubmit={(event) => {
      event.preventDefault();
      const api_keys = bulkKeys(input);
      setClientError(null);
      setSummary(null);
      mutation.clear();
      if (api_keys.length === 0) {
        setClientError('bulkImportEmpty');
        return;
      }
      if (api_keys.length > bulkImportLimit) {
        setClientError('bulkImportTooMany');
        return;
      }
      void mutation.run(async () => {
        const result = await api<BulkKeyImportResponse>(path, { method: 'POST', body: { api_keys } });
        setSummary(result);
        if (result.created === result.results.length) setInput('');
        onSuccess();
      });
    }}>
      <label className="field">{t('bulkImportInput')}
        <textarea name="api_keys" rows={9} value={input} disabled={mutation.busy} spellCheck={false} autoComplete="off" autoFocus
          onChange={(event) => {
            setInput(event.currentTarget.value);
            setClientError(null);
            setSummary(null);
          }} />
      </label>
      <p className="field-help">{t('bulkImportLimit')}</p>
      {clientError && <div className="notice error-notice" role="alert">{t(clientError)}</div>}
      <ErrorNotice error={mutation.error} />
      {summary && <>
        <div className="notice success-notice" role="status">
          {t('bulkImportCreated')} {summary.created} · {t('bulkImportDuplicates')} {summary.duplicates} · {t('bulkImportInvalid')} {summary.invalid}
        </div>
        {summary.created !== summary.results.length && <p className="field-help" role="status">{t('bulkImportRetained')}</p>}
        <ol className="bulk-import-results" aria-label={t('bulkImportResults')}>
          {summary.results.map((result) => <li key={result.index} data-status={result.status}>
            <span>#{result.index + 1}</span><strong>{t(bulkStatusLabel[result.status])}</strong>
          </li>)}
        </ol>
      </>}
      <div className="actions modal-actions">
        <button type="button" className="button" disabled={mutation.busy} onClick={onClose}>{t('bulkImportClose')}</button>
        <button type="submit" className="button button-primary" disabled={mutation.busy}>{t(mutation.busy ? 'loading' : 'bulkImportSubmit')}</button>
      </div>
    </form>
  </Dialog>;
}

function KeyUsageDialog({ path, onClose }: { path: string; onClose: () => void }) {
  const { t } = useLanguage();
  const source = useResource<KeyUsage>(path);
  return <Dialog title={t('upstreamUsage')} onClose={onClose}>
    <p>{t('upstreamUsageHelp')}</p>
    <Resource source={source}>{(snapshot) => <>
      <dl className="definition-list">
        <div><dt>{t('scope')}</dt><dd>{snapshot.authorization_scope}</dd></div>
        <div><dt>{t('status')}</dt><dd><StatusChip status={snapshot.status} /></dd></div>
        <div><dt>{t('upstreamStatus')}</dt><dd>{snapshot.upstream_status}</dd></div>
        <div><dt>{t('usageSnapshotTime')}</dt><dd><Time value={snapshot.fetched_at} /></dd></div>
      </dl>
      {snapshot.usage ? <pre className="json-preview" tabIndex={0}><code>{JSON.stringify(snapshot.usage, null, 2)}</code></pre> : <p>{t('noUsageSnapshot')}</p>}
    </>}</Resource>
  </Dialog>;
}

export function UserManagement({ userId }: { userId?: string }) {
  const { t, locale } = useLanguage();
  const { navigate } = useNavigation();
  const [search, setSearch] = useState('');
  const [creating, setCreating] = useState(false);
  const users = useResource<AdminUser[]>(usersPath);
  const pools = useResource<Pool[]>(poolPath);
  return <>
    <PageHeading title="users" help="userManagementHelp" actions={userId
      ? <Link className="button" href="/admin/users"><ArrowLeft size={16} aria-hidden="true" />{t('backToUsers')}</Link>
      : <button className="button button-primary" type="button" onClick={() => setCreating(true)}><Plus size={17} aria-hidden="true" />{t('createUser')}</button>} />
    {userId ? <Resource source={users}>{(items) => {
      const user = items.find((item) => item.id === userId);
      return user ? <UserDetails key={user.id} user={user} pools={pools} refresh={users.refresh} /> : <Panel><EmptyState title="notFound" /></Panel>;
    }}</Resource>
      : <Panel title={t('users')}>
        <label className="field search-field">{t('searchUsers')}
          <span className="search-input"><Search size={16} aria-hidden="true" /><input type="search" value={search} onChange={(event) => setSearch(event.currentTarget.value)} /></span>
        </label>
        <Resource source={users}>{(items) => {
          const query = search.trim().toLocaleLowerCase(locale);
          const filtered = items.filter((user) => `${user.display_name} ${user.id}`.toLocaleLowerCase(locale).includes(query));
          return filtered.length === 0 ? <EmptyState title="userEmpty" hint="userEmptyHelp" icon={<UsersRound size={22} />} /> :
            <div className="table-scroll"><table className="data-table">
              <thead><tr><th scope="col">{t('user')}</th><th scope="col">{t('status')}</th><th scope="col">{t('pool')}</th><th scope="col">{t('lastLogin')}</th><th scope="col"><span className="sr-only">{t('details')}</span></th></tr></thead>
              <tbody>{filtered.map((user) => {
                const pool = pools.value.state === 'ready' ? pools.value.data.find((item) => item.id === user.upstream_pool_id) : undefined;
                return <tr key={user.id}>
                  <td><strong>{user.display_name}</strong><code className="subline">{user.id}</code></td><td><StatusChip status={user.status} /></td>
                  <td>{pool?.name ?? user.upstream_pool_id ?? t('unassigned')}</td><td><Time value={user.last_login_at} /></td>
                  <td><Link className="button button-sm" href={`/admin/users/${encodeURIComponent(user.id)}`}>{t('details')}</Link></td>
                </tr>;
              })}</tbody>
            </table></div>;
        }}</Resource>
      </Panel>}
    {creating && <NewUser pools={pools} onClose={() => setCreating(false)} onSuccess={(user) => {
      setCreating(false);
      users.refresh();
      navigate(`/admin/users/${encodeURIComponent(user.id)}`);
    }} />}
  </>;
}

type PoolSource = ReturnType<typeof useResource<Pool[]>>;

function NewUser({ pools, onClose, onSuccess }: { pools: PoolSource; onClose: () => void; onSuccess: (user: AdminUser) => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return <Dialog title={t('createUser')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <Resource source={pools}>{(items) => {
      const enabled = items.filter((pool) => pool.enabled);
      return enabled.length === 0 ? <EmptyState title="poolEmpty" hint="poolEmptyHelp" /> : <form onSubmit={(event) => {
        event.preventDefault();
        const fields = new FormData(event.currentTarget);
        const display_name = formText(fields, 'display_name');
        const upstream_pool_id = formText(fields, 'pool');
        const oauth_subject = formText(fields, 'oauth_subject') || undefined;
        if (display_name && upstream_pool_id) void mutation.run(async () => {
          onSuccess(await api<AdminUser>(usersPath, { method: 'POST', body: { display_name, upstream_pool_id, oauth_subject } }));
        });
      }}>
        <label className="field">{t('displayName')}<input name="display_name" required maxLength={120} autoFocus /></label>
        <label className="field">{t('poolAssignment')}<select name="pool" required defaultValue="">
          <option value="" disabled>{t('unassigned')}</option>{enabled.map((pool) => <option key={pool.id} value={pool.id}>{pool.name}</option>)}
        </select></label>
        <label className="field">{t('oauthSubject')}<input name="oauth_subject" maxLength={128} spellCheck={false} /></label>
        <p className="field-help">{t('oauthSubjectHelp')}</p>
        <ErrorNotice error={mutation.error} />
        <FormActions busy={mutation.busy} submit="create" onClose={onClose} />
      </form>;
    }}</Resource>
  </Dialog>;
}

function UserDetails({ user, pools, refresh }: { user: AdminUser; pools: PoolSource; refresh: () => void }) {
  const { t } = useLanguage();
  const userPath = `${usersPath}/${encodeURIComponent(user.id)}`;
  const assignment = useResource<QuotaAssignment>(`${userPath}/quota`);
  const mutation = useMutation();
  const [disable, setDisable] = useState(false);
  const [quota, setQuota] = useState(false);
  const [assignedPool, setAssignedPool] = useState(user.upstream_pool_id ?? '');
  async function setStatus(status: string) {
    await api<void>(userPath, { method: 'PATCH', body: { status } });
    refresh();
  }
  return <>
    <Panel title={user.display_name} actions={<>
      <StatusChip status={user.status} />
      <button className={`button${user.status === 'active' ? ' button-text-danger' : ''}`} type="button" disabled={mutation.busy}
        onClick={() => user.status === 'active' ? setDisable(true) : void mutation.run(() => setStatus('active'))}>{t(user.status === 'active' ? 'disable' : 'enable')}</button>
      <Link className="button" href={`/admin/logs?user_id=${encodeURIComponent(user.id)}`}>{t('requests')}</Link>
    </>}>
      <dl className="definition-list">
        <div><dt>{t('userId')}</dt><dd><code>{user.id}</code></dd></div><div><dt>{t('created')}</dt><dd><Time value={user.created_at} /></dd></div>
        <div><dt>{t('lastLogin')}</dt><dd><Time value={user.last_login_at} /></dd></div>
      </dl>
      <Resource source={pools}>{(items) => <form className="inline-form" onSubmit={(event) => {
        event.preventDefault();
        if (!assignedPool || assignedPool === user.upstream_pool_id) return;
        void mutation.run(async () => {
          await api<void>(userPath, { method: 'PATCH', body: { upstream_pool_id: assignedPool } });
          refresh();
        });
      }}>
        <label className="field">{t('poolAssignment')}<select value={assignedPool} onChange={(event) => setAssignedPool(event.currentTarget.value)} required>
          <option value="" disabled>{t('unassigned')}</option>
          {items.map((pool) => <option key={pool.id} value={pool.id} disabled={!pool.enabled}>{pool.name}</option>)}
        </select></label>
        <button className="button" type="submit" disabled={mutation.busy || !assignedPool || assignedPool === user.upstream_pool_id}>{t('save')}</button>
      </form>}</Resource>
      <ErrorNotice error={mutation.error} />
    </Panel>
    <Panel title={t('quota')} actions={<button type="button" className="button" disabled={assignment.value.state !== 'ready'} onClick={() => setQuota(true)}>{t('quotaEdit')}</button>}>
      <Resource source={assignment}>{(value) => <QuotaOverview assignment={value} />}</Resource>
    </Panel>
    <Panel title={t('manageTokens')}><TokenManager path={`${userPath}/tokens`} disabled={user.status !== 'active'} /></Panel>
    {disable && <Confirm title="disableUserTitle" help="disableUserHelp" yes="disable" onClose={() => setDisable(false)} onConfirm={() => setStatus('disabled')} />}
    {quota && assignment.value.state === 'ready' && <EditQuota path={`${userPath}/quota`} assignment={assignment.value.data} onClose={() => setQuota(false)} onSuccess={() => { setQuota(false); assignment.refresh(); }} />}
  </>;
}

const limitFields = {
  requests_per_five_minutes: 'fiveMinutes', business_calls_per_hour: 'perHour', daily_units: 'dailyUnits', monthly_units: 'monthlyUnits',
} satisfies Record<keyof QuotaLimits, Text>;
const businessOperations = operations.filter((operation) => operation === 'mcp_tools_call' || !operation.startsWith('mcp_'));

function EditQuota({ path, assignment, onClose, onSuccess }: { path: string; assignment: QuotaAssignment; onClose: () => void; onSuccess: () => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  const plan = assignment.plan;
  return <Dialog title={t('quotaEdit')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <p>{t('quotaLimitHelp')}</p>
    <form onSubmit={(event) => {
      event.preventDefault();
      const fields = new FormData(event.currentTarget);
      const limits = Object.fromEntries(quotaLimits.map((key) => [key, Number(fields.get(key))]));
      const weights = Object.fromEntries(businessOperations.flatMap((operation) => {
        const value = formText(fields, operation);
        return value ? [[operation, Number(value)]] : [];
      }));
      void mutation.run(async () => {
        await api(path, { method: 'PATCH', body: { ...limits, weights, expected_version: plan?.version ?? 0 } });
        onSuccess();
      });
    }}>
      <div className="filter-grid">{quotaLimits.map((key) => <label className="field" key={key}>{t(limitFields[key])}
        <input type="number" min="0" max="1000000000" step="1" name={key} defaultValue={plan?.[key] ?? ''} required />
      </label>)}</div>
      <fieldset className="weights">
        <legend>{t('operationWeights')}</legend><p className="field-help">{t('weightHelp')}</p>
        {businessOperations.map((operation) => <label className="field inline-field" key={operation}>
          <code>{operation}</code><input type="number" min="1" max="1000" step="1" name={operation} defaultValue={plan?.weights[operation] ?? ''} placeholder="1" />
        </label>)}
      </fieldset>
      <ErrorNotice error={mutation.error} />
      <FormActions busy={mutation.busy} submit="save" onClose={onClose} />
    </form>
  </Dialog>;
}

export function AnnouncementsPage() {
  const { t } = useLanguage();
  const path = '/api/admin/announcements';
  const notices = useResource<Announcement[]>(path);
  const mutation = useMutation();
  const [creating, setCreating] = useState(false);
  return <>
    <PageHeading title="announcements" help="announcementHelp" actions={<button className="button button-primary" type="button" onClick={() => setCreating(true)}><Plus size={17} aria-hidden="true" />{t('addAnnouncement')}</button>} />
    <Panel title={t('announcements')}>
      <ErrorNotice error={mutation.error} />
      <Resource source={notices}>{(items) => items.length === 0 ? <EmptyState title="announcementEmpty" hint="announcementEmptyHelp" icon={<Megaphone size={22} />} /> :
        <div className="announcement-list">{items.map((item) => <article key={item.id}>
          <div><h3>{item.title}</h3><p>{item.body}</p>
            <p className="muted"><Time value={item.created_at} />{item.ends_at && <> · {t('expiration')} <Time value={item.ends_at} /></>}</p>
          </div>
          <div className="actions">
            <StatusChip status={item.enabled ? item.ends_at && item.ends_at * 1000 <= Date.now() ? 'ended' : 'enabled' : 'disabled'} />
            <button type="button" className="button button-sm" disabled={mutation.busy} onClick={() => void mutation.run(async () => {
              await api<void>(`${path}/${encodeURIComponent(item.id)}`, { method: 'PATCH', body: { enabled: !item.enabled } });
              notices.refresh();
            })}>{t(item.enabled ? 'disable' : 'enable')}</button>
          </div>
        </article>)}</div>}
      </Resource>
    </Panel>
    {creating && <NewAnnouncement path={path} onClose={() => setCreating(false)} onSuccess={() => { setCreating(false); notices.refresh(); }} />}
  </>;
}

function NewAnnouncement({ path, onClose, onSuccess }: { path: string; onClose: () => void; onSuccess: () => void }) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return <Dialog title={t('addAnnouncement')} onClose={() => { if (!mutation.busy) onClose(); }}>
    <form onSubmit={(event) => {
      event.preventDefault();
      const fields = new FormData(event.currentTarget);
      const title = formText(fields, 'title');
      const body = formText(fields, 'body');
      const end = formText(fields, 'ends_at');
      const ends_at = end ? Math.floor(Date.parse(end) / 1000) : undefined;
      if (title && body) void mutation.run(async () => {
        await api(path, { method: 'POST', body: { title, body, ends_at, enabled: true } });
        onSuccess();
      });
    }}>
      <label className="field">{t('title')}<input name="title" required maxLength={120} autoFocus /></label>
      <label className="field">{t('body')}<textarea name="body" required rows={5} maxLength={4000} /></label>
      <label className="field">{t('endsAt')}<input type="datetime-local" name="ends_at" /></label>
      <ErrorNotice error={mutation.error} />
      <FormActions busy={mutation.busy} submit="publish" onClose={onClose} />
    </form>
  </Dialog>;
}

export function SettingsPage() {
  const { t } = useLanguage();
  const path = '/api/admin/settings';
  const settings = useResource<Settings>(path);
  const mutation = useMutation();
  const [saved, setSaved] = useState(false);
  return <>
    <PageHeading title="settings" help="settingsHelp" />
    <Panel title={t('registration')}>
      <Resource source={settings}>{(data) => <form onSubmit={(event) => {
        event.preventDefault();
        const fields = new FormData(event.currentTarget);
        const registration_open = fields.get('registration_open') === 'on';
        setSaved(false);
        void mutation.run(async () => {
          await api<void>(path, { method: 'PATCH', body: { registration_open } });
          settings.refresh();
          setSaved(true);
        });
      }}>
        <label className="checkbox-field"><input type="checkbox" name="registration_open" defaultChecked={data.registration_open} />{t('registrationHelp')}</label>
        <button className="button button-primary" type="submit" disabled={mutation.busy}>{t(mutation.busy ? 'loading' : 'save')}</button>
      </form>}</Resource>
      <ErrorNotice error={mutation.error} />
      {saved && <p className="muted" role="status">{t('saved')}</p>}
    </Panel>
    <PasskeySettings />
  </>;
}

export function FormActions({ busy, submit, onClose }: { busy: boolean; submit: Text; onClose: () => void }) {
  const { t } = useLanguage();
  return <div className="actions modal-actions">
    <button type="button" className="button" disabled={busy} onClick={onClose}>{t('cancel')}</button>
    <button type="submit" className="button button-primary" disabled={busy}>{t(busy ? 'loading' : submit)}</button>
  </div>;
}

function formText(fields: FormData, key: string) {
  return String(fields.get(key) ?? '').trim();
}

function bulkKeys(input: string) {
  return input.split('\n').map((line) => line.trim()).filter(Boolean);
}

export function AdminBilling() {
  const { locale, t } = useLanguage();
  const path = '/api/admin/billing/codes';
  const [created, setCreated] = useState<{ id: string; code: string } | null>(null);
  const [userFilter, setUserFilter] = useState('');
  const [appliedUserFilter, setAppliedUserFilter] = useState('');
  const [reversing, setReversing] = useState<Order | null>(null);
  const mutation = useMutation();
  const orderPath = appliedUserFilter ? `/api/admin/orders?user_id=${encodeURIComponent(appliedUserFilter)}` : '/api/admin/orders';
  const orders = useResource<Order[]>(orderPath);
  const ledgerPath = appliedUserFilter ? `/api/admin/billing/ledger?user_id=${encodeURIComponent(appliedUserFilter)}` : '/api/admin/billing/ledger';
  const ledger = useResource<LedgerEntry[]>(ledgerPath);
  return <>
    <PageHeading title="billingCodes" help="billingCodesHelp" />
    <Panel title={t('issueRedemptionCode')}>
      <form onSubmit={(event) => {
        event.preventDefault();
        const fields = new FormData(event.currentTarget);
        const units = Number(fields.get('units'));
        const valid_days = Number(fields.get('valid_days'));
        const expires_in_days = Number(fields.get('expires_in_days'));
        if ([units, valid_days, expires_in_days].every((value) => Number.isInteger(value) && value > 0)) {
          void mutation.run(async () => setCreated(await api(path, { method: 'POST', body: { units, valid_days, expires_in_days } })));
        }
      }}>
        <div className="filter-grid">
          <label className="field">{t('credits')}<input name="units" type="number" min="1" max="1000000000" required /></label>
          <label className="field">{t('validDays')}<input name="valid_days" type="number" min="1" max="365" required /></label>
          <label className="field">{t('codeExpiryDays')}<input name="expires_in_days" type="number" min="1" max="365" required /></label>
        </div>
        <ErrorNotice error={mutation.error} />
        <div className="actions form-actions"><button className="button button-primary" type="submit" disabled={mutation.busy}>{t(mutation.busy ? 'loading' : 'issueRedemptionCode')}</button></div>
      </form>
      {created && <div className="notice success-notice"><div><strong>{t('redemptionCodeIssued')}</strong></div>
        <button className="button button-sm" type="button" onClick={() => setCreated(null)}>{t('close')}</button>
      </div>}
      {created && <CopyBlock text={created.code} label={t('redemptionCode')} />}
      {created && <p className="muted">{t('oneTimeCodeWarning')}</p>}
    </Panel>
    <Panel title={t('orders')}>
      <form className="inline-form" onSubmit={(event) => { event.preventDefault(); setAppliedUserFilter(userFilter.trim()); }}>
        <label className="field">{t('userId')}<input value={userFilter} onChange={(event) => setUserFilter(event.currentTarget.value)} maxLength={120} /></label>
        <button className="button" type="submit">{t('apply')}</button>
        <button className="button" type="button" onClick={() => { setUserFilter(''); setAppliedUserFilter(''); }}>{t('reset')}</button>
      </form>
      <Resource source={orders}>{(items) => items.length === 0 ? <EmptyState title="ordersEmpty" /> : <div className="table-scroll">
        <table className="data-table"><thead><tr><th scope="col">{t('created')}</th><th scope="col">{t('userId')}</th><th scope="col">{t('amount')}</th><th scope="col">{t('credits')}</th><th scope="col">{t('status')}</th><th scope="col">{t('orderId')}</th><th scope="col"><span className="sr-only">{t('reverseOrder')}</span></th></tr></thead>
          <tbody>{items.map((order) => <tr key={order.id}><td><Time value={order.created_at} /></td><td><Link href={`/admin/users/${encodeURIComponent(order.user_id ?? '')}`}>{order.user_id ?? '—'}</Link></td>
            <td>{price(order.amount_minor, order.currency, locale)}</td><td>{order.units}</td><td><StatusChip status={order.status === 'pending' ? 'pendingPayment' : order.status} /></td><td><code>{order.id}</code></td>
            <td>{order.status === 'settled' && <button className="button button-sm button-text-danger" type="button" onClick={() => setReversing(order)}>{t('reverseOrder')}</button>}</td></tr>)}</tbody>
        </table>
      </div>}</Resource>
    </Panel>
    <Panel title={t('creditLedger')}>
      <Resource source={ledger}>{(items) => items.length === 0 ? <EmptyState title="ledgerEmpty" /> : <div className="table-scroll">
        <table className="data-table"><thead><tr><th scope="col">{t('created')}</th><th scope="col">{t('userId')}</th><th scope="col">{t('ledgerAction')}</th><th scope="col">{t('credits')}</th><th scope="col">{t('expiration')}</th><th scope="col">{t('orderId')}</th></tr></thead>
          <tbody>{items.map((entry) => <tr key={entry.id}><td><Time value={entry.created_at} /></td><td><Link href={`/admin/users/${encodeURIComponent(entry.user_id)}`}>{entry.user_id}</Link></td>
            <td><StatusChip status={entry.kind} /></td><td>{entry.units}</td><td><Time value={entry.expires_at} /></td><td><code>{entry.reference_id}</code></td></tr>)}</tbody>
        </table>
      </div>}</Resource>
    </Panel>
    {reversing && <Confirm title="reverseOrderTitle" help="reverseOrderHelp" yes="reverseOrder" onClose={() => setReversing(null)} onConfirm={async () => {
      await api<void>(`/api/admin/orders/${encodeURIComponent(reversing.id)}/reverse`, { method: 'POST' });
      setReversing(null);
      orders.refresh();
      ledger.refresh();
    }} />}
  </>;
}
