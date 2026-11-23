import { Activity, Download, Gauge, KeyRound, RefreshCw, ShieldCheck } from 'lucide-react';
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
    <PageHeading
      title="dashboard"
      help={admin ? 'adminDashboardHelp' : 'dashboardHelp'}
      actions={!admin && (
        <Link className="button button-primary" href="/console/tokens">
          <KeyRound size={17} aria-hidden="true" />
          {t('manageTokens')}
        </Link>
      )}
    />

    {admin ? (
      <Resource source={adminUsage}>{({ totals, daily, pools }) => <>
        <Stats totals={totals} admin={true} />
        <div className="workbench-layout">
          <div className="workbench-box" style={{ margin: 0 }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline', marginBottom: '16px', paddingBottom: '12px', borderBottom: '1px solid var(--line)' }}>
              <strong style={{ fontFamily: 'var(--font-display)', fontSize: '15px', color: 'var(--ink)' }}>上游 Context7 密钥池状态矩阵</strong>
              <Link className="text-link" href="/admin/pools">{t('pools')} →</Link>
            </div>
            <div className="bench-table-wrap" style={{ margin: 0 }}>
              <table className="bench-table">
                <thead>
                  <tr>
                    <th>上游池指标项</th>
                    <th>当前计数 / 状态</th>
                    <th>运行健康度</th>
                  </tr>
                </thead>
                <tbody>
                  <tr>
                    <td><strong>{t('activePools')}</strong></td>
                    <td>{pools.enabled} / {pools.total} 已启用</td>
                    <td><span className="key-badge-status"><span className="status-dot-emerald"></span> NORMAL</span></td>
                  </tr>
                  <tr>
                    <td><strong>{t('availableKeys')}</strong></td>
                    <td>{pools.available_keys} 个上游密钥就绪</td>
                    <td><span className="key-badge-status"><span className="status-dot-emerald"></span> READY</span></td>
                  </tr>
                  <tr>
                    <td><strong>{t('unavailableKeys')}</strong></td>
                    <td>{pools.unavailable_keys} 个故障/熔断密钥</td>
                    <td>
                      <span style={{ color: pools.unavailable_keys > 0 ? 'var(--bad)' : 'var(--quiet)', fontWeight: 600 }}>
                        {pools.unavailable_keys > 0 ? 'GUARD TRIGGERED' : 'NONE'}
                      </span>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <div style={{ marginTop: '14px', fontSize: '12.5px', color: 'var(--quiet)' }}>
              网关根据各上游密钥权重轮询分配请求，自动阻断上游 429 熔断插槽。
            </div>
          </div>

          <div className="workbench-box bench-bg" style={{ margin: 0 }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline', marginBottom: '16px', paddingBottom: '12px', borderBottom: '1px solid var(--line)' }}>
              <strong style={{ fontFamily: 'var(--font-display)', fontSize: '15px', color: 'var(--ink)' }}>网关系统基础设施</strong>
              <span className="key-badge-status"><span className="status-dot-emerald"></span> OPERATIONAL</span>
            </div>
            <Resource source={health}>{(data) => (
              <dl className="definition-list" style={{ margin: 0 }}>
                <div><dt>{t('databaseState')}</dt><dd><StatusChip status={data.database === 'ok' ? 'available' : 'unavailable'} /></dd></div>
                <div><dt>{t('releaseVersion')}</dt><dd><code style={{ color: 'var(--brand)' }}>{data.version}</code></dd></div>
                <div><dt>服务架构</dt><dd>Rust Tokio Edge Gateway (Streamable SSE)</dd></div>
                <div><dt>调度引擎</dt><dd>动态加权健康插槽熔断轮询</dd></div>
              </dl>
            )}</Resource>
          </div>
        </div>
        <UsageChart daily={daily} />
        <LatencyPanel totals={totals} />
      </>}</Resource>
    ) : (
      <>
        {announcements.value.state === 'ready' && announcements.value.data.length > 0 &&
          <div className="announcements" aria-label={t('announcements')}>
            {announcements.value.data.map((item) => (
              <section key={item.id} className="notice announcement">
                <ShieldCheck size={18} aria-hidden="true" />
                <div><strong>{item.title}</strong><p>{item.body}</p></div>
              </section>
            ))}
          </div>}

        <Resource source={userUsage}>{(usage) => <>
          <Stats totals={usage.totals} admin={false} />
          <div className="workbench-layout">
            <DashboardClientQuickConnect />
            <div className="workbench-box" style={{ margin: 0, display: 'flex', flexDirection: 'column', justifyContent: 'space-between' }}>
              <div>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'baseline', marginBottom: '16px', paddingBottom: '12px', borderBottom: '1px solid var(--line)' }}>
                  <strong style={{ fontFamily: 'var(--font-display)', fontSize: '15px', color: 'var(--ink)' }}>个人多维流控雷达</strong>
                  <span className="key-badge-status"><span className="status-dot-emerald"></span> 等级配额已就绪 · 登录即用</span>
                </div>
                <QuotaRadarTable assignment={usage} />
              </div>
              <div style={{ marginTop: '20px', padding: '14px', border: '1px solid var(--line)', background: 'var(--bench)', borderRadius: '4px', display: 'flex', justifyContent: 'space-between', alignItems: 'center', fontSize: '12.5px' }}>
                <Resource source={status}>{(data) => (
                  <>
                    <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                      <StatusChip status={data.status} />
                      <span style={{ color: 'var(--subtle)' }}>按信任等级每日/每月自动刷新 · 无需手动充值</span>
                    </div>
                    <strong style={{ color: 'var(--brand)', fontFamily: 'var(--font-mono)' }}>{data.active_tokens} 个网关密钥就绪</strong>
                  </>
                )}</Resource>
              </div>
            </div>
          </div>
          <UsageChart daily={usage.daily} />
          <LatencyPanel totals={usage.totals} />
        </>}</Resource>
      </>
    )}

    <div className="grid-section-header" style={{ marginTop: '32px' }}>
      <h2 className="grid-section-title">{admin ? '全站实时调用审计流' : '实时调用日志流'}</h2>
      <Link className="text-link" href={history} style={{ fontSize: '12.5px', fontWeight: 700 }}>
        {t('viewAll')} →
      </Link>
    </div>
    <Resource source={recent}>{(data) => <EventsTable events={data.items} admin={admin} />}</Resource>
  </>;
}

function DashboardClientQuickConnect() {
  const [client, setClient] = useState<'cursor' | 'claude' | 'codex' | 'windsurf' | 'rest'>('cursor');
  const [copied, setCopied] = useState(false);
  const origin = typeof window !== 'undefined' ? window.location.origin : 'http://localhost:8787';
  const mcpUrl = `${origin}/mcp`;

  const guides: Record<string, { file: string; code: string }> = {
    cursor: {
      file: '~/.cursor/mcp.json (Streamable HTTP)',
      code: JSON.stringify({
        mcpServers: {
          context7_gateway: {
            url: mcpUrl,
            headers: { Authorization: "Bearer ${env:CONTEXT7_GATEWAY_TOKEN}" }
          }
        }
      }, null, 2)
    },
    claude: {
      file: 'claude_desktop_config.json',
      code: JSON.stringify({
        mcpServers: {
          context7_gateway: {
            command: "npx",
            args: ["-y", "mcp-remote@0.16.7", mcpUrl, "--transport", "http-only", "--header", "Authorization:${GATEWAY_AUTH_HEADER}"]
          }
        }
      }, null, 2)
    },
    codex: {
      file: '~/.codex/config.toml',
      code: `[mcp_servers.context7_gateway]\nurl = "${mcpUrl}"\nbearer_token_env_var = "CONTEXT7_GATEWAY_TOKEN"`
    },
    windsurf: {
      file: '~/.codeium/windsurf/mcp_config.json',
      code: JSON.stringify({
        mcpServers: {
          context7_gateway: {
            serverUrl: mcpUrl,
            headers: { Authorization: "Bearer ${env:CONTEXT7_GATEWAY_TOKEN}" }
          }
        }
      }, null, 2)
    },
    rest: {
      file: 'Terminal · cURL REST Probe',
      code: `curl -s "${origin}/api/v3/search?query=Tokio&type=json" \\\n  -H "Authorization: Bearer <GATEWAY_TOKEN>"`
    }
  };

  const current = guides[client];
  const copyCode = async () => {
    try {
      await navigator.clipboard.writeText(current.code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {}
  };

  return (
    <div className="client-spec-block">
      <div className="spec-tab-bar">
        {(['cursor', 'claude', 'codex', 'windsurf', 'rest'] as const).map((key) => (
          <button
            key={key}
            type="button"
            className={`spec-tab${client === key ? ' active' : ''}`}
            onClick={() => setClient(key)}
          >
            {key.toUpperCase()}
          </button>
        ))}
      </div>
      <div className="spec-content-view">
        <div className="spec-file-banner">
          <span style={{ fontWeight: 600, color: 'var(--subtle)' }}>{current.file}</span>
          <button type="button" className="button button-sm button-primary" onClick={copyCode}>
            {copied ? '✓ 已复制配置' : '一键复制代码'}
          </button>
        </div>
        <pre className="code-swiss-print">{current.code}</pre>
        <div style={{ marginTop: '14px', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <span style={{ fontSize: '12px', color: 'var(--quiet)' }}>粘贴后在本地配置 Token 环境变量即可直连</span>
          <Link className="text-link" href="/console/setup">进入完整向导 →</Link>
        </div>
      </div>
    </div>
  );
}

function Stats({ totals, admin = false }: { totals: UsageTotals; admin?: boolean }) {
  const { locale, t } = useLanguage();
  const format = new Intl.NumberFormat(locale);
  const totalReq = totals.requests;
  const successRate = totalReq > 0 ? ((totals.successful / totalReq) * 100).toFixed(2) : '100.00';
  return <div className="big-number-grid">
    <div className="big-number-cell">
      <div className="cell-subhead">
        <span>{t('requestsLast30Days')}</span>
        <span style={{ color: 'var(--brand)' }}>30D TOTAL</span>
      </div>
      <div className="cell-num">{format.format(totalReq)}</div>
      <div className="cell-foot">{t('lastThirtyDays')} · 累计调用次数</div>
    </div>
    <div className="big-number-cell">
      <div className="cell-subhead">
        <span>{t('successful')}</span>
        <span style={{ color: 'var(--good)' }}>TARGET ≥99.9%</span>
      </div>
      <div className="cell-num" style={{ color: 'var(--good)' }}>{successRate}%</div>
      <div className="cell-foot">{format.format(totals.successful)} 次请求正常响应</div>
    </div>
    <div className="big-number-cell">
      <div className="cell-subhead">
        <span>{admin ? t('credits') : '等级配额消耗'}</span>
        <span>{admin ? 'GLOBAL UNITS' : 'TIER QUOTA'}</span>
      </div>
      <div className="cell-num">{format.format(totals.units)}</div>
      <div className="cell-foot">{admin ? '全站累计消耗算力积分' : '按信任等级自动分配 · 登录即用'}</div>
    </div>
    <div className="big-number-cell">
      <div className="cell-subhead">
        <span>{t('latencyP95')}</span>
        <span>STREAM SSE</span>
      </div>
      <div className="cell-num">
        {totals.latency_p95_ms === null ? '—' : format.format(totals.latency_p95_ms)}
        <span style={{ fontSize: '20px', fontWeight: 600 }}>ms</span>
      </div>
      <div className="cell-foot">中位数首包 ~{totals.latency_p50_ms ?? 48}ms</div>
    </div>
  </div>;
}

export function QuotaRadarTable({ assignment }: { assignment: QuotaAssignment }) {
  const { locale } = useLanguage();
  const format = new Intl.NumberFormat(locale).format;
  if (!assignment.plan) {
    return <EmptyState title="noPlan" hint="noPlanHelp" icon={<Gauge size={22} />} />;
  }

  const windowsConfig = [
    { key: 'requests_5m' as const, label: '5 分钟突发限速', unit: '次' },
    { key: 'business_1h' as const, label: '1 小时业务窗口', unit: '次' },
    { key: 'daily' as const, label: '每日配额上限 (按等级分配)', unit: 'CR' },
    { key: 'monthly' as const, label: '每月配额上限 (按等级分配)', unit: 'CR' },
  ];

  return (
    <div className="bench-table-wrap" style={{ margin: 0 }}>
      <table className="bench-table">
        <thead>
          <tr>
            <th>限流与配额窗口</th>
            <th>当前已用 / 等级上限</th>
            <th>占用比例</th>
            <th>限速状态</th>
          </tr>
        </thead>
        <tbody>
          {windowsConfig.map(({ key, label, unit }) => {
            const win = assignment.windows[key];
            const used = win ? win.used + win.reserved : 0;
            const limit = win?.limit ?? null;
            const hasLimit = limit !== null && limit > 0;
            const ratio = hasLimit ? used / limit : 0;
            const percentStr = hasLimit ? `${(ratio * 100).toFixed(1)}%` : '—';
            let badgeClass = 'status-badge-radar good';
            let badgeText = key.includes('daily') || key.includes('monthly') ? '正常消耗' : '充裕畅通';
            if (hasLimit && ratio >= 1.0) {
              badgeClass = 'status-badge-radar bad';
              badgeText = '触发限流';
            } else if (hasLimit && ratio >= 0.85) {
              badgeClass = 'status-badge-radar warn';
              badgeText = '临界预警';
            }
            return (
              <tr key={key}>
                <td><strong>{label}</strong></td>
                <td>{format(used)} / {hasLimit ? `${format(limit)} ${unit}` : '无限制'}</td>
                <td>{percentStr}</td>
                <td>
                  <span className={badgeClass}>
                    <span
                      className="status-dot-emerald"
                      style={{
                        background: badgeClass.includes('bad') ? 'var(--bad)' : badgeClass.includes('warn') ? 'var(--warn)' : 'var(--good)',
                      }}
                    />{' '}
                    {badgeText}
                  </span>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
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
        <div className="bench-table-wrap"><table className="bench-table">
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
  const quickFilter = (statusVal: string) => {
    const updated = { ...filters, status: statusVal };
    setFilters(updated);
    setApplied(updated);
    setPage(0);
    setCursors(['']);
    setExported(false);
  };

  return <>
    <PageHeading title="requests" help={admin ? 'adminLogsHelp' : 'requestHistoryHelp'} actions={<button className="button" type="button" onClick={events.refresh}>
      <RefreshCw size={16} aria-hidden="true" />{t('refresh')}
    </button>} />
    <Panel title={t('filters')} actions={
      <div style={{ display: 'flex', gap: '8px' }}>
        <button type="button" className={`btn-swiss-action btn-swiss-secondary${applied.status === '' ? ' active' : ''}`} style={{ padding: '4px 10px', fontSize: '11px' }} onClick={() => quickFilter('')}>全部记录</button>
        <button type="button" className={`btn-swiss-action btn-swiss-secondary${applied.status === 'success' ? ' active' : ''}`} style={{ padding: '4px 10px', fontSize: '11px' }} onClick={() => quickFilter('success')}>仅成功 (200)</button>
        <button type="button" className={`btn-swiss-action btn-swiss-secondary${applied.status === 'failure' ? ' active' : ''}`} style={{ padding: '4px 10px', fontSize: '11px' }} onClick={() => quickFilter('failure')}>仅拦截 / 429</button>
      </div>
    }>
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
    <div className="bench-table-wrap" style={{ margin: 0 }}>
      <table className="bench-table">
        <thead><tr>
          <th>{t('time')}</th>
          <th>路由与方法</th>
          <th>操作行为</th>
          <th>响应状态</th>
          <th>耗时</th>
          <th>配额积分</th>
          <th>操作</th>
        </tr></thead>
        <tbody>{events.map((event) => {
          const isSuccess = !event.failure_code && (!event.gateway_status || event.gateway_status < 400);
          return (
            <tr key={event.id}>
              <td><Time value={event.started_at} /></td>
              <td><code>{event.operation.startsWith('mcp_') ? 'POST /mcp' : 'GET /v3'}</code></td>
              <td><strong>{operationName(event.operation, locale)}</strong></td>
              <td>
                <span style={{
                  color: isSuccess ? 'var(--good)' : 'var(--bad)',
                  fontWeight: 700,
                  fontFamily: 'var(--font-mono)'
                }}>
                  {isSuccess ? `${event.gateway_status || 200} OK` : (event.failure_code ?? `${event.gateway_status} ERR`)}
                </span>
              </td>
              <td>{event.duration_ms} ms</td>
              <td>{event.success_units} CR</td>
              <td>
                <button
                  type="button"
                  className="btn-swiss-action btn-swiss-secondary"
                  style={{ padding: '3px 8px', fontSize: '11px' }}
                  onClick={() => setSelected(event)}
                >
                  {t('details')}
                </button>
              </td>
            </tr>
          );
        })}</tbody>
      </table>
    </div>
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
