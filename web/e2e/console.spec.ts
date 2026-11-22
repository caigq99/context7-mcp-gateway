import { expect, test } from '@playwright/test';

const user = { id: 'user-1', display_name: 'Demo user', status: 'active' };
const pool = { id: 'pool-1', name: 'Primary', authorization_scope: 'Teamspace north', enabled: true, created_at: '2026-11-20T00:00:00Z' };
const upstreamKey = {
  id: 'upstream-1', pool_id: 'pool-1', display_prefix: 'ctx7_a…', status: 'active',
  created_at: '2026-11-20T00:00:00Z', last_checked_at: null, last_used_at: null,
};
const plan = {
  version: 2, requests_per_five_minutes: 30, business_calls_per_hour: 50,
  daily_units: 800, monthly_units: 12000, weights: { mcp_tools_call: 2 }, created_at: '2026-11-20T00:00:00Z',
};
const totals = {
  requests: 43, successful: 40, failed: 3, units: 84, quota_rejections: 2,
  latency_p50_ms: 40, latency_p95_ms: 180, latency_p99_ms: 300,
};
const daily = Array.from({ length: 30 }, (_, offset) => ({
  date: new Date(Date.now() - (29 - offset) * 86400000).toISOString().slice(0, 10),
  requests: offset === 29 ? 3 : 0, successful: offset === 29 ? 3 : 0, failed: 0, units: offset === 29 ? 6 : 0,
}));
const event = {
  id: 'event-1', gateway_token_id: 'token-1', operation: 'mcp_tools_call', request_id: 'request-1',
  started_at: '2026-11-21T10:00:00Z', duration_ms: 42, gateway_status: 200,
  upstream_status: 200, success_units: 2, failure_code: null,
};
const baseToken = {
  id: 'token-1', name: 'Codex', prefix: 'c7gw_abc…', status: 'active',
  created_at: '2026-11-20T00:00:00Z', last_used_at: null, expires_at: null,
  scopes: ['mcp', 'context7:read'],
};

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('context7.locale', 'en'));
  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    const json = (body: unknown, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    if (path === '/api/user/me') return json({ user });
    if (path === '/api/user/tokens' && method === 'POST') return json({ ...baseToken, id: 'token-2', name: 'New client', token: 'c7gw_one_time_secret' }, 201);
    if (path === '/api/user/tokens' || path === '/api/admin/users/user-1/tokens') return json([baseToken]);
    if (path === '/api/user/usage/summary') return json({ plan, windows: {
      requests_5m: { used: 4, limit: 30, reserved: 0, resets_at: '2026-11-21T10:05:00Z' },
      business_1h: { used: 9, limit: 50, reserved: 0, resets_at: '2026-11-21T11:00:00Z' },
      daily: { used: 22, limit: 800, reserved: 2, resets_at: '2026-11-22T00:00:00Z' },
      monthly: { used: 84, limit: 12000, reserved: 2, resets_at: '2026-12-01T00:00:00Z' },
    }, totals, daily });
    if (path === '/api/admin/usage/summary') return json({ totals, daily, pools: { total: 1, enabled: 1, available_keys: 1, unavailable_keys: 0 } });
    if (path === '/api/user/usage/events' || path === '/api/admin/logs') return json({ items: [event], next_cursor: null });
    if (path === '/api/user/service-status') return json({ status: 'available', active_tokens: 1 });
    if (path === '/api/user/setup-guides') return json({ origin: 'https://gateway.example.test', mcp_url: 'https://gateway.example.test/mcp', rest_origin: 'https://gateway.example.test/api/context7', token_env: 'GATEWAY_API_KEY' });
    if (path === '/api/user/announcements') return json([{ id: 'notice-1', title: 'Maintenance', body: 'Brief gateway update', enabled: true, created_at: '2026-11-21T00:00:00Z', ends_at: null }]);
    if (path === '/api/admin/auth/session') return json({ authenticated: true, passkey_available: true });
    if (path === '/api/admin/auth/passkeys') return json([{ id: 'passkey-1', name: 'Laptop key', created_at: 1795219200, last_used_at: null }]);
    if (path === '/api/admin/pools' && method === 'GET') return json([pool]);
    if (path === '/api/admin/pools/pool-1/keys') return json([upstreamKey]);
    if (path === '/api/admin/pools/pool-1/keys/upstream-1/usage') return json({ authorization_scope: pool.authorization_scope, fetched_at: '2026-11-21T10:00:00Z', status: 'available', upstream_status: 200, usage: { requests_today: 41 } });
    if (path === '/api/admin/users') return json([{ ...user, upstream_pool_id: 'pool-1', created_at: '2026-11-20T00:00:00Z', last_login_at: null }]);
    if (path === '/api/admin/users/user-1/quota' && method === 'GET') return json({ plan, windows: {
      requests_5m: { used: 4, limit: 30, reserved: 0, resets_at: null },
      business_1h: { used: 9, limit: 50, reserved: 0, resets_at: null },
      daily: { used: 22, limit: 800, reserved: 0, resets_at: null },
      monthly: { used: 84, limit: 12000, reserved: 0, resets_at: null },
    } });
    if (path === '/api/admin/settings') return json({ registration_open: false });
    if (path === '/api/admin/announcements') return json([]);
    if (path === '/api/user/billing') return json({ enabled: false, quotes: [], balance: 0 });
    if (path === '/api/user/orders' || path === '/api/admin/orders') return json([]);
    if (path === '/health/ready') return json({ status: 'ready', database: 'ok', version: '0.1.0' });
    if (method === 'DELETE' || method === 'PATCH' || method === 'POST') return route.fulfill({ status: 204 });
    return json({ error: 'not_found' }, 404);
  });
});

test('unauthenticated user sees Linux.do sign-in and no private console data', async ({ page }) => {
  await page.route('**/api/user/me', (route) => route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({ error: 'unauthorized' }) }));
  await page.goto('/console');
  await expect(page.getByRole('link', { name: 'Sign in with Linux.do' })).toHaveAttribute('href', '/auth/linuxdo');
  await expect(page.getByRole('link', { name: 'Administrator access' })).toBeVisible();
});

test('overview renders real quota status, usage and request activity', async ({ page }) => {
  await page.goto('/console/dashboard');
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
  await expect(page.getByText('Available', { exact: true })).toBeVisible();
  await expect(page.getByText('Requests / 5 min')).toBeVisible();
  await expect(page.getByRole('cell', { name: 'MCP · Tool call' })).toBeVisible();
  await expect(page.getByText('Maintenance')).toBeVisible();
});

test('new Gateway key appears once and can be copied', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.goto('/console/tokens');
  await page.getByRole('button', { name: 'Create API key' }).click();
  await page.getByLabel('API key name').fill('New client');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('c7gw_one_time_secret');
  await page.getByRole('dialog').getByRole('button', { name: 'Copy' }).click();
  await expect(page.getByRole('dialog')).toContainText('Copied');
  await expect(page.getByRole('dialog')).toContainText('shown only here');
});

test('REST and MCP client templates use the deployment origin and a placeholder', async ({ page }) => {
  await page.goto('/console/setup');
  await expect(page.getByText('https://gateway.example.test/mcp', { exact: true })).toBeVisible();
  await expect(page.getByText('GATEWAY_API_KEY', { exact: false }).first()).toBeVisible();
  const client = page.getByLabel('Client');
  await client.selectOption('rest');
  await expect(page.getByText('/v3/search', { exact: false })).toBeVisible();
  await expect(page.getByText('API key', { exact: false }).first()).toBeVisible();
  const endpoint = page.getByLabel('REST endpoint');
  const output = page.locator('.code-block pre').last();
  for (const [operation, path] of [
    ['fetch_v3', '/v3/fetch'], ['library_search_v2', '/v2/libs/search'], ['context_v2', '/v2/context'],
  ]) {
    await endpoint.selectOption(operation);
    await expect(output).toContainText(path);
  }
});

test('request history applies operation, result and UTC date filters', async ({ page }) => {
  let observedSearch = '';
  await page.route('**/api/user/usage/events?*', async (route) => {
    observedSearch = new URL(route.request().url()).search;
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ items: [event], next_cursor: null }) });
  });
  await page.goto('/console/requests');
  await page.getByLabel('Operation').selectOption('mcp_tools_call');
  await page.getByLabel('Status').selectOption('success');
  await page.getByLabel('From · UTC').fill('2026-11-21');
  await page.getByRole('button', { name: 'Apply filters' }).click();
  await expect.poll(() => observedSearch).toContain('from=1795219200');
  await expect(observedSearch).toContain('operation=mcp_tools_call');
  await expect(observedSearch).toContain('status=success');
});

test('quota errors are readable on Gateway API requests', async ({ page }) => {
  await page.route('**/api/user/tokens', async (route) => {
    if (route.request().method() === 'GET') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([baseToken]) });
    return route.fulfill({ status: 429, contentType: 'application/json', body: JSON.stringify({ error: 'quota_exceeded' }) });
  });
  await page.goto('/console/tokens');
  await page.getByRole('button', { name: 'Create API key' }).click();
  await page.getByLabel('API key name').fill('Quota test');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('exceeded the gateway’s local quota');
});

test('admin can inspect pool usage and disable an upstream key', async ({ page }) => {
  let statusBody: Record<string, unknown> | null = null;
  await page.route('**/api/admin/pools/pool-1/keys/upstream-1/status', async (route) => {
    statusBody = route.request().postDataJSON() as Record<string, unknown>;
    await route.fulfill({ status: 204 });
  });
  await page.goto('/admin/pools/pool-1');
  await expect(page.getByText('ctx7_a…')).toBeVisible();
  await page.getByRole('button', { name: 'Upstream usage' }).click();
  await expect(page.getByRole('dialog')).toContainText('Teamspace north');
  await page.getByRole('button', { name: 'Close' }).click();
  await page.getByRole('button', { name: 'Disable', exact: true }).last().click();
  await page.getByRole('dialog').getByRole('button', { name: 'Disable', exact: true }).click();
  await expect.poll(() => statusBody).toEqual({ status: 'disabled' });
});

test('admin bulk imports upstream keys without echoing raw values', async ({ page }) => {
  const keys = ['ctx7sk-alpha', 'ctx7sk-beta', 'ctx7sk-gamma'];
  let method = '';
  let payload: unknown = null;
  await page.route('**/api/admin/pools/pool-1/keys/bulk', async (route) => {
    method = route.request().method();
    payload = route.request().postDataJSON();
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
      created: 1,
      duplicates: 1,
      invalid: 1,
      results: [
        { index: 0, status: 'created', key_id: 'key-a', display_prefix: 'ctx7_ab…' },
        { index: 1, status: 'duplicate' },
        { index: 2, status: 'invalid' },
      ],
    }) });
  });
  await page.goto('/admin/pools/pool-1');
  await page.getByRole('button', { name: 'Bulk import' }).click();
  const dialog = page.getByRole('dialog');
  const input = dialog.getByLabel('Context7 API keys');
  const originalInput = `  ${keys[0]}  \n\n${keys[1]}\n${keys[2]}\n`;
  await input.fill(originalInput);
  await dialog.getByRole('button', { name: 'Import keys' }).click();
  await expect.poll(() => method).toBe('POST');
  await expect.poll(() => payload).toEqual({ api_keys: keys });
  const summary = dialog.locator('.success-notice');
  await expect(summary).toContainText('Created 1 · Duplicates 1 · Invalid 1');
  for (const key of keys) await expect(summary).not.toContainText(key);
  await expect(input).toHaveValue(originalInput);
  const lineResults = dialog.getByRole('list', { name: 'Line results' });
  await expect(lineResults.getByRole('listitem')).toHaveCount(3);
  await expect(lineResults).toContainText('#1');
  await expect(lineResults).toContainText('Created');
  await expect(lineResults).toContainText('Duplicate');
  await expect(lineResults).toContainText('Invalid');
  const close = dialog.getByRole('button', { name: 'Close', exact: true }).last();
  await expect(close).toBeVisible();
  await close.click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('admin edits quota with an optimistic plan version', async ({ page }) => {
  let change: Record<string, unknown> | null = null;
  await page.route('**/api/admin/users/user-1/quota', async (route) => {
    if (route.request().method() === 'GET') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ plan, windows: {
      requests_5m: { used: 4, limit: 30, reserved: 0, resets_at: null },
      business_1h: { used: 9, limit: 50, reserved: 0, resets_at: null },
      daily: { used: 22, limit: 800, reserved: 0, resets_at: null },
      monthly: { used: 84, limit: 12000, reserved: 0, resets_at: null },
    } }) });
    change = route.request().postDataJSON() as Record<string, unknown>;
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(plan) });
  });
  await page.goto('/admin/users/user-1');
  await page.getByRole('button', { name: 'Edit quota' }).click();
  await page.getByRole('spinbutton', { name: 'Requests / 5 min' }).fill('24');
  await page.getByRole('dialog').getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => change).toMatchObject({ requests_per_five_minutes: 24, expected_version: 2 });
});

test('redeem a one-time local credit code without enabling payment', async ({ page }) => {
  await page.goto('/console/billing');
  await expect(page.getByText('Credit purchases are unavailable')).toBeVisible();
  await page.getByRole('button', { name: 'Redeem a credit code' }).click();
  await page.getByLabel('One-time redemption code').fill('c7rc_mock_value');
  await page.getByRole('dialog').getByRole('button', { name: 'Redeem a credit code' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('admin can inspect the credit ledger and reverse an eligible order', async ({ page }) => {
  const order = {
    id: 'order-1', user_id: 'user-1', quote_id: 'credit-50', quote_version: 1, units: 50,
    valid_days: 30, amount_minor: 500, currency: 'USD', status: 'settled', created_at: '2026-11-20T00:00:00Z',
    expires_at: '2026-12-01T00:00:00Z', settled_at: '2026-11-20T00:00:00Z',
  };
  let reversed = false;
  await page.route('**/api/admin/orders', async (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([reversed ? { ...order, status: 'reversed' } : order]) }));
  await page.route('**/api/admin/billing/ledger', async (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([{
    id: 'ledger-1', user_id: 'user-1', lot_id: 'lot-1', kind: 'grant', reference_id: 'order-1', units: 50,
    created_at: '2026-11-20T00:00:00Z', expires_at: '2026-12-20T00:00:00Z',
  }]) }));
  await page.route('**/api/admin/orders/order-1/reverse', async (route) => {
    reversed = true;
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ ...order, status: 'reversed' }) });
  });
  await page.goto('/admin/billing');
  await expect(page.getByText('order-1').first()).toBeVisible();
  await expect(page.getByText('Granted', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Reverse settled order' }).click();
  await page.getByRole('dialog').getByRole('button', { name: 'Reverse settled order' }).click();
  await expect.poll(() => reversed).toBe(true);
});

test('mobile navigation and keyboard focus remain usable', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/console/dashboard');
  const menu = page.getByRole('button', { name: 'Menu' });
  await expect(menu).toBeVisible();
  await menu.focus();
  await expect(menu).toBeFocused();
  await menu.click();
  await expect(page.getByRole('navigation', { name: 'Console navigation' })).toBeVisible();
  await page.getByRole('navigation', { name: 'Console navigation' }).getByRole('link', { name: 'Client setup' }).click();
  await expect(page).toHaveURL(/\/console\/setup$/);
  await expect(page.getByRole('heading', { name: 'Client setup' })).toBeVisible();
});
