export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly requestId: string | null,
  ) {
    super(code);
    this.name = 'ApiError';
  }
}

type ApiOptions = {
  method?: 'GET' | 'POST' | 'PATCH' | 'DELETE';
  body?: unknown;
  signal?: AbortSignal;
};

export async function api<T>(path: string, options: ApiOptions = {}): Promise<T> {
  const response = await fetch(path, {
    ...options,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    credentials: 'same-origin',
    cache: 'no-store',
    headers: {
      Accept: 'application/json',
      ...(options.body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
  });
  if (!response.ok) {
    const payload: unknown = await response.json().catch(() => null);
    const code = payload && typeof payload === 'object' && 'error' in payload && typeof payload.error === 'string'
      ? payload.error
      : 'request_failed';
    throw new ApiError(response.status, code, response.headers.get('x-gateway-request-id'));
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

export type User = {
  id: string;
  display_name: string;
  status: string;
};

export type AdminUser = User & {
  upstream_pool_id: string | null;
  last_login_at: string | null;
  created_at: string;
};

export type GatewayToken = {
  id: string;
  name: string;
  prefix: string;
  status?: string;
  token?: string | null;
  created_at: string;
  last_used_at?: string | null;
  expires_at?: string | null;
  scopes?: GatewayScope[];
};

export type GatewayScope = 'mcp' | 'context7:read';

export type Health = {
  status: string;
  version: string;
  database: string;
};

export type Pool = {
  id: string;
  name: string;
  authorization_scope: string;
  enabled: boolean;
  created_at: string;
};

export type UpstreamKey = {
  id: string;
  pool_id: string;
  display_prefix: string;
  status: string;
  created_at: string;
  last_checked_at: string | null;
  last_used_at: string | null;
};

export type KeyUsage = {
  fetched_at: string;
  authorization_scope: string;
  status: string;
  upstream_status: number;
  usage: Record<string, unknown> | null;
};

export type Announcement = {
  id: string;
  title: string;
  body: string;
  enabled: boolean;
  created_at: string;
  ends_at: number | null;
};

export type Settings = {
  registration_open: boolean;
};

export type ServiceStatus = { status: 'available' | 'unavailable'; active_tokens: number };

export const operations = [
  'mcp_tools_call',
  'mcp_initialize',
  'mcp_tools_list',
  'mcp_notification',
  'mcp_ping',
  'mcp_stream',
  'mcp_close',
  'mcp_other',
  'search_v3',
  'fetch_v3',
  'library_search_v2',
  'context_v2',
] as const;

export type Operation = (typeof operations)[number];

export type UsageEvent = {
  id: string;
  gateway_token_id: string;
  operation: string;
  request_id: string;
  started_at: string;
  duration_ms: number;
  gateway_status: number;
  upstream_status: number | null;
  success_units: number;
  failure_code: string | null;
  user_id?: string;
  pool_id?: string | null;
  upstream_key_id?: string | null;
};

export type EventPage = {
  items: UsageEvent[];
  next_cursor: string | null;
};

export const quotaLimits = [
  'requests_per_five_minutes',
  'business_calls_per_hour',
  'daily_units',
  'monthly_units',
] as const;

export type QuotaLimits = Record<(typeof quotaLimits)[number], number>;
export type QuotaPlan = QuotaLimits & {
  version: number;
  weights: Partial<Record<Operation, number>>;
  created_at: string;
};
export type QuotaWindow = {
  used: number;
  limit: number;
  reserved: number;
  resets_at: string | null;
};
export type QuotaAssignment = {
  plan: QuotaPlan | null;
  windows: Record<'requests_5m' | 'business_1h' | 'daily' | 'monthly', QuotaWindow>;
};

export type DailyUsage = {
  date: string;
  requests: number;
  successful: number;
  failed: number;
  units: number;
};

export type UsageTotals = Omit<DailyUsage, 'date'> & {
  quota_rejections: number;
  latency_p50_ms: number | null;
  latency_p95_ms: number | null;
  latency_p99_ms: number | null;
};

export type UserUsage = QuotaAssignment & {
  totals: UsageTotals;
  daily: DailyUsage[];
};

export type AdminUsage = {
  totals: UsageTotals;
  daily: DailyUsage[];
  pools: { total: number; enabled: number; available_keys: number; unavailable_keys: number };
};

export type GuideEndpoints = {
  origin: string;
  mcp_url: string;
  rest_origin: string;
  token_env: 'GATEWAY_API_KEY';
};

export type BillingQuote = {
  id: string;
  version: number;
  label: string;
  units: number;
  valid_days: number;
  amount_minor: number;
  currency: string;
};

export type BillingStatus = {
  enabled: boolean;
  quotes: BillingQuote[];
  balance: number;
  redemption_enabled?: boolean;
};

export type Order = {
  id: string;
  quote_id: string;
  quote_version: number;
  units: number;
  valid_days: number;
  amount_minor: number;
  currency: string;
  status: string;
  created_at: string;
  expires_at: string;
  settled_at: string | null;
  user_id?: string;
  payment_url?: string | null;
};

export type LedgerEntry = {
  id: string;
  user_id: string;
  lot_id: string;
  kind: string;
  reference_id: string;
  units: number;
  created_at: string;
  expires_at: string;
};

export type OrderPreview = {
  quote: BillingQuote;
  balance: number;
  projected_balance: number;
  applies_to: 'monthly';
};
