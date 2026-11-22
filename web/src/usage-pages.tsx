import { Activity, ArrowRight, BookOpenText, Download, Gauge, KeyRound, RefreshCw, ShieldCheck } from 'lucide-react';
import { useState } from 'react';
import { ApiError, operations } from './api';
import type { AdminUsage, Announcement, DailyUsage, EventPage, GatewayToken, Health, QuotaAssignment, QuotaWindow, ServiceStatus, UsageEvent, UsageTotals, UserUsage } from './api';
import { CopyBlock, Dialog, EmptyState, ErrorNotice, PageHeading, Panel, Resource, StatusChip, Time, useMutation, useResource } from './components';
import { useLanguage } from './i18n';
import type { Text } from './i18n';
import { Link, useNavigation } from './navigation';

const quotaWindows = {
  requests_5m: 'fiveMinutes',
  business_1h: 'perHour',
  daily: 'dailyUnits',
  monthly: 'monthlyUnits',
} satisfies Record<keyof QuotaAssignment['windows'], Text>;

export function Stat({ label, value, caption, Icon = Activity }: {
  label: Text;
  value: number | string | null;
  caption?: Text;
  Icon?: typeof Activity;
}) {
  const { locale, t } = useLanguage();
  return <article className="stat-card">
    <div className="stat-label"><span>{t(label)}</span><Icon size={17} aria-hidden="true" /></div>
    <strong>{typeof value === 'number' ? new Intl.NumberFormat(locale).format(value) : value ?? '—'}</strong>
    {caption && <span className="stat-caption">{t(caption)}</span>}
  </article>;
}

export function Dashboard({ admin = false }: { admin?: boolean }) {
  const { t } = useLanguage();
  const userUsage = useResource<UserUsage>(admin ? null : '/api/user/usage/summary');
  const adminUsage = useResource<AdminUsage>(admin ? '/api/admin/usage/summary' : null);
  const status = useResource<ServiceStatus>(admin ? null : '/api/user/service-status');
  const health = useResource<Health>(admin ? '/health/ready' : null);
  const announcements = useResource<Announcement[]>(admin ? null : '/api/user/announcements');
  const recent = useResource<EventPage>(admin ? '/api/admin/logs?limit=5' : '/api/user/usage/events?limit=5');
  const history = admin ? '/admin/logs' : '/console/requests';
  return <>
    <PageHeading title="dashboard" help={admin ? 'adminDashboardHelp' : 'dashboardHelp'} actions={!admin &&
      <Link className="button button-primary" href="/console/tokens"><KeyRound size={17} aria-hidden="true" />{t('manageTokens')}</Link>}
    />
    {admin ? <Resource source={adminUsage}>{({ totals, daily, pools }) => <>
      <Stats totals={totals} />
      <div className="dashboard-grid">
        <Panel title={t('poolHealth')} actions={<Link className="text-link" href="/admin/pools">{t('pools')}<ArrowRight size={15} aria-hidden="true" /></Link>}>
          <dl className="definition-list">
            <div><dt>{t('activePools')}</dt><dd>{pools.enabled} / {pools.total}</dd></div>
            <div><dt>{t('availableKeys')}</dt><dd>{pools.available_keys}</dd></div>
            <div><dt>{t('unavailableKeys')}</dt><dd>{pools.unavailable_keys}</dd></div>
          </dl>
        </Panel>
        <Panel title={t('gateway')}>
          <Resource source={health}>{(data) => <dl className="definition-list">
            <div><dt>{t('databaseState')}</dt><dd><StatusChip status={data.database === 'ok' ? 'available' : 'unavailable'} /></dd></div>
            <div><dt>{t('releaseVersion')}</dt><dd><code>{data.version}</code></dd></div>
          </dl>}</Resource>
        </Panel>
      </div>
      <UsageChart daily={daily} />
      <LatencyPanel totals={totals} />
    </>}</Resource> : <>
      {announcements.value.state === 'ready' && announcements.value.data.length > 0 &&
        <div className="announcements" aria-label={t('announcements')}>
          {announcements.value.data.map((item) => <section key={item.id} className="notice announcement"><ShieldCheck size={18} aria-hidden="true" />
            <div><strong>{item.title}</strong><p>{item.body}</p></div>
          </section>)}
        </div>}
      <Resource source={userUsage}>{(usage) => <>
        <Stats totals={usage.totals} />
        <div className="dashboard-grid quota-grid">
          <Panel title={t('quota')} hint={usage.plan ? `${t('quotaPlanVersion')} ${usage.plan.version}` : undefined}>
            <QuotaOverview assignment={usage} />
          </Panel>
          <Panel title={t('gateway')}>
            <Resource source={status}>{(data) => <>
              <StatusChip status={data.status} />
              <p>{t(data.status === 'available' ? 'statusAvailableHelp' : 'statusUnavailableHelp')}</p>
              <dl className="definition-list"><div><dt>{t('activeGatewayKeys')}</dt><dd>{data.active_tokens}</dd></div></dl>
            </>}</Resource>
            <Link className="text-link" href="/console/setup"><BookOpenText size={16} aria-hidden="true" />{t('setup')}<ArrowRight size={14} aria-hidden="true" /></Link>
          </Panel>
        </div>
        <UsageChart daily={usage.daily} />
        <LatencyPanel totals={usage.totals} />
      </>}</Resource>
    </>}
    <Panel title={t('recentRequests')} actions={<Link className="text-link" href={history}>{t('viewAll')}<ArrowRight size={15} aria-hidden="true" /></Link>}>
      <Resource source={recent}>{(data) => <EventsTable events={data.items} admin={admin} />}</Resource>
    </Panel>
  </>;
}

function Stats({ totals }: { totals: UsageTotals }) {
  const { t } = useLanguage();
  return <div className="stats-grid" aria-label={t('lastThirtyDays')}>
    <Stat label="requestsLast30Days" value={totals.requests} caption="lastThirtyDays" />
    <Stat label="successful" value={totals.successful} caption="lastThirtyDays" Icon={ShieldCheck} />
    <Stat label="credits" value={totals.units} caption="lastThirtyDays" Icon={Gauge} />
    <Stat label="quotaRejections" value={totals.quota_rejections} caption="lastThirtyDays" Icon={KeyRound} />
  </div>;
}

function LatencyPanel({ totals }: { totals: UsageTotals }) {
  const { locale, t } = useLanguage();
  const format = new Intl.NumberFormat(locale);
  const latencyLabels = { p50: 'latencyP50', p95: 'latencyP95', p99: 'latencyP99' } as const;
  return <Panel title={t('latency')} hint={t('lastThirtyDays')}>
    <div className="latency-grid">
      {(['p50', 'p95', 'p99'] as const).map((percentile) => {
        const value = totals[`latency_${percentile}_ms`];
        const label = latencyLabels[percentile];
        return <div key={percentile} className="latency-stat">
          <span>{t(label)}</span><strong>{value === null ? '—' : format.format(value)}</strong>
        </div>;
      })}
    </div>
  </Panel>;
}

export function QuotaOverview({ assignment }: { assignment: QuotaAssignment }) {
  if (!assignment.plan) return <EmptyState title="noPlan" hint="noPlanHelp" icon={<Gauge size={22} />} />;
  return <div className="quota-windows">{Object.entries(quotaWindows).map(([key, label]) =>
    <QuotaMeter key={key} label={label} window={assignment.windows[key as keyof typeof quotaWindows]} />)}</div>;
}

function QuotaMeter({ label, window }: { label: Text; window: QuotaWindow }) {
  const { locale, t } = useLanguage();
  const count = new Intl.NumberFormat(locale).format;
  const occupied = window.used + window.reserved;
  const hasLimit = window.limit !== null;
  const full = hasLimit && occupied >= window.limit;
  return <div className={`quota-meter${full ? ' at-limit' : ''}`}>
    <div className="quota-topline"><strong>{t(label)}</strong><span>{count(window.used)} / {hasLimit ? count(window.limit) : t('quotaUnlimited')}</span></div>
    {hasLimit && <meter min={0} max={Math.max(window.limit, 1)} value={window.limit === 0 ? 1 : Math.min(occupied, window.limit)}
      aria-label={t(label)} aria-valuetext={`${count(occupied)} / ${count(window.limit)}`} />}
    <div className="quota-meta">
      {window.reserved > 0 && <span>{count(window.reserved)} {t('reserved')}</span>}
      <span>{window.resets_at ? <>{t('resetAt')} <Time value={window.resets_at} /></> : t('resetUnavailable')}</span>
    </div>
    {full && <small className="quota-warning">{t('quotaLimitedHelp')}</small>}
  </div>;
}

function UsageChart({ daily }: { daily: DailyUsage[] }) {
  const { locale, t } = useLanguage();
  const format = new Intl.NumberFormat(locale);
  const lastSevenDays = daily.slice(-7);
  const scale = Math.max(1, ...lastSevenDays.map((day) => day.requests));
  const [legend, setLegend] = useState<'requests' | 'units'>('requests');
  const axis = legend === 'requests' ? scale : Math.max(1, ...lastSevenDays.map((day) => day.units));
  const hasActivity = lastSevenDays.some((day) => day.requests > 0 || day.units > 0);
  const shortDate = new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric', timeZone: 'UTC' });
  return <Panel title={t('dailyUsage')} hint={t('lastSevenDays')} actions={<div className="segmented">
    <button type="button" className="button button-sm" aria-pressed={legend === 'requests'} onClick={() => setLegend('requests')}>{t('displayRequests')}</button>
          <button type="button" className="button button-sm" aria-pressed={legend === 'units'} onClick={() => setLegend('units')}>{t('credits')}</button>
  </div>}>
    {hasActivity ? <>
      <div className="usage-chart" role="img" aria-label={`${t('dailyUsage')}. ${t('chartHelp')}`}>
        {lastSevenDays.map((day) => <div className="chart-column" key={day.date} aria-hidden="true" title={`${day.date} · ${format.format(day[legend])}`}>
          <div className="bar-track"><div className="bar-value" style={{ '--bar-height': `${day[legend] / axis * 100}%` } as React.CSSProperties} /></div>
          <span>{shortDate.format(new Date(`${day.date}T00:00:00Z`))}</span>
        </div>)}
      </div>
      <details className="chart-details"><summary>{t('showData')}</summary>
        <div className="table-scroll"><table className="data-table">
          <thead><tr><th scope="col">{t('date')}</th><th scope="col">{t('requestsCount')}</th><th scope="col">{t('successful')}</th><th scope="col">{t('failed')}</th><th scope="col">{t('credits')}</th></tr></thead>
          <tbody>{lastSevenDays.map((day) => <tr key={day.date}>
            <th scope="row"><Time value={day.date} dateOnly utc /></th><td>{format.format(day.requests)}</td><td>{format.format(day.successful)}</td><td>{format.format(day.failed)}</td><td>{format.format(day.units)}</td>
          </tr>)}</tbody>
        </table></div>
      </details>
    </> : <EmptyState title="noActivity" icon={<Activity size={22} />} />}
  </Panel>;
}

const labels: Record<string, [string, string]> = {
  mcp_tools_call: ['MCP · Tool call', 'MCP · 工具调用'], mcp_initialize: ['MCP · Initialize', 'MCP · 初始化'],
  mcp_tools_list: ['MCP · Tool list', 'MCP · 列出工具'], mcp_notification: ['MCP · Notification', 'MCP · 通知'],
  mcp_ping: ['MCP · Ping', 'MCP · Ping'], mcp_stream: ['MCP · Stream', 'MCP · 流读取'],
  mcp_close: ['MCP · Close session', 'MCP · 关闭会话'], mcp_other: ['MCP · Other', 'MCP · 其他'],
  search_v3: ['REST · Search', 'REST · 文档搜索'], fetch_v3: ['REST · Fetch', 'REST · 读取页面'],
  library_search_v2: ['REST · Library search', 'REST · 库搜索'], context_v2: ['REST · Context', 'REST · 文档上下文'],
};

function operationName(operation: string, locale: string) {
  return labels[operation]?.[locale === 'zh-CN' ? 1 : 0] ?? operation;
}

export function RequestPage({ admin = false }: { admin?: boolean }) {
  const { locale, t } = useLanguage();
  const { location } = useNavigation();
  const initialToken = location.searchParams.get('token_id') ?? '';
  const initialUser = location.searchParams.get('user_id') ?? '';
  return <RequestBrowser key={`${admin}:${initialToken}:${initialUser}`} admin={admin} initialToken={initialToken} initialUser={initialUser} locale={locale} t={t} />;
}

function RequestBrowser({ admin, initialToken, initialUser, locale, t }: {
  admin: boolean;
  initialToken: string;
  initialUser: string;
  locale: string;
  t: (key: Text) => string;
}) {
  const [filters, setFilters] = useState({ token_id: initialToken, user_id: admin ? initialUser : '', operation: '', status: '', from: '', to: '', request_id: '' });
  const [applied, setApplied] = useState(filters);
  const [page, setPage] = useState(0);
  const [cursors, setCursors] = useState(['']);
  const [exported, setExported] = useState(false);
  const download = useMutation();
  const tokens = useResource<GatewayToken[]>(admin ? null : '/api/user/tokens');
  const endpoint = admin ? '/api/admin/logs' : '/api/user/usage/events';
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(applied)) {
    if (!value.trim()) continue;
    if (key === 'from' || key === 'to') {
      const utcTime = key === 'from' ? 'T00:00:00Z' : 'T23:59:59Z';
      query.set(key, String(Math.floor(Date.parse(value + utcTime) / 1000)));
    } else {
      query.set(key, value.trim());
    }
  }
  const exportQuery = query.toString();
  if (cursors[page]) query.set('cursor', cursors[page]);
  query.set('limit', '25');
  const events = useResource<EventPage>(`${endpoint}?${query}`);
  function clearFilters() {
    const empty = { token_id: '', user_id: '', operation: '', status: '', from: '', to: '', request_id: '' };
    setFilters(empty);
    setApplied(empty);
    setPage(0);
    setCursors(['']);
  }
  return <>
    <PageHeading title="requests" help={admin ? 'adminLogsHelp' : 'requestHistoryHelp'} actions={<button className="button" type="button" onClick={events.refresh}>
      <RefreshCw size={16} aria-hidden="true" />{t('refresh')}
    </button>} />
    <Panel title={t('filters')}>
      <form onSubmit={(event) => {
        event.preventDefault();
        setApplied(filters);
        setPage(0);
        setCursors(['']);
        setExported(false);
      }}>
        <div className="filter-grid">
          <label className="field">{t('operation')}<select value={filters.operation} onChange={(event) => setFilters({ ...filters, operation: event.currentTarget.value })}>
            <option value="">{t('allOperations')}</option>
            {operations.map((operation) => <option value={operation} key={operation}>{operationName(operation, locale)}</option>)}
          </select></label>
          <label className="field">{t('status')}<select value={filters.status} onChange={(event) => setFilters({ ...filters, status: event.currentTarget.value })}>
            <option value="">{t('allStatuses')}</option><option value="success">{t('conditionSuccess')}</option><option value="failure">{t('conditionFailed')}</option>
          </select></label>
          <label className="field">{t('requestId')}<input value={filters.request_id} maxLength={120} onChange={(event) => setFilters({ ...filters, request_id: event.currentTarget.value })} /></label>
          {admin ? <>
            <label className="field">{t('userId')}<input value={filters.user_id} maxLength={120} onChange={(event) => setFilters({ ...filters, user_id: event.currentTarget.value })} /></label>
            <label className="field">{t('tokenId')}<input value={filters.token_id} maxLength={120} onChange={(event) => setFilters({ ...filters, token_id: event.currentTarget.value })} /></label>
          </> : <label className="field">{t('key')}<select value={filters.token_id} onChange={(event) => setFilters({ ...filters, token_id: event.currentTarget.value })}>
            <option value="">{t('allKeys')}</option>
            {filters.token_id && (tokens.value.state !== 'ready' || !tokens.value.data.some((item) => item.id === filters.token_id)) &&
              <option value={filters.token_id}>{filters.token_id}</option>}
            {tokens.value.state === 'ready' && tokens.value.data.map((item) => <option key={item.id} value={item.id}>{item.name} · {item.prefix}</option>)}
          </select></label>}
          <label className="field">{t('from')} · UTC<input type="date" value={filters.from} max={filters.to || undefined} onChange={(event) => setFilters({ ...filters, from: event.currentTarget.value })} /></label>
          <label className="field">{t('to')} · UTC<input type="date" value={filters.to} min={filters.from || undefined} onChange={(event) => setFilters({ ...filters, to: event.currentTarget.value })} /></label>
        </div>
        <div className="actions form-actions">
          <button type="submit" className="button button-primary">{t('apply')}</button>
          <button type="button" className="button" onClick={clearFilters}>{t('reset')}</button>
        </div>
      </form>
    </Panel>
    <Panel title={t('requests')} actions={admin && <button className="button" type="button" title={t('exportLogsHelp')} disabled={download.busy}
      onClick={() => void download.run(async () => { await downloadEvents(exportQuery); setExported(true); })}>
      <Download size={16} aria-hidden="true" />{t(download.busy ? 'loading' : 'exportLogs')}
    </button>}>
      <ErrorNotice error={download.error} />
      {exported && <p className="muted" role="status">{t('exportDone')}</p>}
      <Resource source={events}>{(data) => <>
        <EventsTable events={data.items} admin={admin} />
        {(page > 0 || data.next_cursor) && <nav className="pagination" aria-label={t('page')}>
          <button className="button" type="button" disabled={page === 0} onClick={() => setPage(page - 1)}>{t('previous')}</button>
          <span>{t('page')} {page + 1}</span>
          <button className="button" type="button" disabled={!data.next_cursor} onClick={() => {
            if (!data.next_cursor) return;
            setCursors((prior) => [...prior.slice(0, page + 1), data.next_cursor!]);
            setPage(page + 1);
          }}>{t('next')}</button>
        </nav>}
      </>}</Resource>
    </Panel>
  </>;
}

async function downloadEvents(query: string) {
  const response = await fetch(`/api/admin/logs/export${query ? `?${query}` : ''}`, {
    cache: 'no-store', credentials: 'same-origin', headers: { Accept: 'application/x-ndjson' },
  });
  if (!response.ok) throw new ApiError(response.status, 'request_failed', response.headers.get('x-gateway-request-id'));
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement('a');
  link.href = url;
  link.download = 'gateway-requests.ndjson';
  document.body.appendChild(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function EventsTable({ events, admin = false }: { events: UsageEvent[]; admin?: boolean }) {
  const { locale, t } = useLanguage();
  const [selected, setSelected] = useState<UsageEvent | null>(null);
  if (events.length === 0) return <EmptyState title="eventsEmpty" hint="eventsEmptyHelp" icon={<Activity size={22} />} />;
  return <>
    <div className="table-scroll"><table className="data-table">
      <thead><tr>
        <th scope="col">{t('time')}</th><th scope="col">{t('operation')}</th><th scope="col">{t('failure')}</th>
        <th scope="col">{t('latency')}</th><th scope="col">{t('credits')}</th><th scope="col"><span className="sr-only">{t('details')}</span></th>
      </tr></thead>
      <tbody>{events.map((event) => <tr key={event.id}>
        <td><Time value={event.started_at} /></td><td>{operationName(event.operation, locale)}</td>
        <td><StatusChip status={event.failure_code ? 'failure' : 'success'} />
          <code className="subline">{event.failure_code ?? event.gateway_status}</code>
        </td>
        <td>{event.duration_ms} ms</td><td>{event.success_units}</td>
        <td><button className="button button-sm" type="button" onClick={() => setSelected(event)}>{t('details')}</button></td>
      </tr>)}</tbody>
    </table></div>
    {selected && <Dialog title={t('requestDetail')} onClose={() => setSelected(null)}>
      <CopyBlock text={selected.request_id} label={t('requestId')} />
      <dl className="definition-list">
        <div><dt>{t('operation')}</dt><dd>{operationName(selected.operation, locale)}</dd></div>
        <div><dt>{t('time')}</dt><dd><Time value={selected.started_at} /></dd></div>
        <div><dt>{t('tokenId')}</dt><dd><code>{selected.gateway_token_id}</code></dd></div>
        {admin && <>
          <div><dt>{t('userId')}</dt><dd><code>{selected.user_id ?? '—'}</code></dd></div>
          <div><dt>{t('pool')}</dt><dd><code>{selected.pool_id ?? '—'}</code></dd></div>
          <div><dt>{t('upstreamKeyId')}</dt><dd><code>{selected.upstream_key_id ?? '—'}</code></dd></div>
        </>}
        <div><dt>{t('gatewayStatus')}</dt><dd>{selected.gateway_status}</dd></div>
        <div><dt>{t('upstreamStatus')}</dt><dd>{selected.upstream_status ?? '—'}</dd></div>
        <div><dt>{t('resultCode')}</dt><dd><code>{selected.failure_code ?? '—'}</code></dd></div>
        <div><dt>{t('latency')}</dt><dd>{selected.duration_ms} ms</dd></div>
        <div><dt>{t('credits')}</dt><dd>{selected.success_units}</dd></div>
      </dl>
    </Dialog>}
  </>;
}
