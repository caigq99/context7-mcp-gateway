import { CreditCard, Receipt, TicketCheck } from 'lucide-react';
import { useState } from 'react';
import { api } from './api';
import type { BillingStatus, Order, OrderPreview } from './api';
import { Confirm, Dialog, EmptyState, ErrorNotice, Resource, StatusChip, Time, useMutation, useResource } from './components';
import { useLanguage } from './i18n';

export function price(amount_minor: number, currency: string, locale: string) {
  return new Intl.NumberFormat(locale, { style: 'currency', currency: currency.toUpperCase() }).format(amount_minor / 100);
}

export function BillingPage() {
  const { locale, t } = useLanguage();
  const status = useResource<BillingStatus>('/api/user/billing');
  const orders = useResource<Order[]>('/api/user/orders');
  const redemption = useMutation();
  const [codeDialog, setCodeDialog] = useState(false);
  const [inlineCode, setInlineCode] = useState('');
  const [selectedQuote, setSelectedQuote] = useState<BillingStatus['quotes'][number] | null>(null);
  const [preview, setPreview] = useState<OrderPreview | null>(null);
  const [created, setCreated] = useState<Order | null>(null);

  const handleInlineRedeem = (e: React.FormEvent) => {
    e.preventDefault();
    const code = inlineCode.trim();
    if (!code) return;
    void redemption.run(async () => {
      await api('/api/user/billing/redeem', { method: 'POST', body: { code } });
      setInlineCode('');
      status.refresh();
      orders.refresh();
    });
  };

  return <>
    <div className="grid-section-header">
      <div>
        <h2 className="grid-section-title">配额积分与账单中心</h2>
        <span className="grid-section-meta">本地原子额度账本与充值结算</span>
      </div>
      <button className="btn-swiss-action" type="button" onClick={() => setCodeDialog(true)}>
        <TicketCheck size={16} aria-hidden="true" />
        {t('redeemCode')}
      </button>
    </div>

    <Resource source={status}>{(data) => <>
      {!data.enabled && (
        <section className="notice warning-notice" role="status" style={{ marginBottom: '20px' }}>
          <CreditCard size={18} aria-hidden="true" />
          <div><strong>{t('billingDisabled')}</strong><p>{t('billingDisabledHelp')}</p></div>
        </section>
      )}

      {/* 双栏工程台：兑换与权益明细 */}
      <div className="workbench-layout">
        <div className="workbench-box" style={{ margin: 0 }}>
          <h3 style={{ fontSize: '15px', fontWeight: 800, fontFamily: 'var(--font-display)', marginBottom: '8px', color: 'var(--ink)' }}>
            兑换配额充值码
          </h3>
          <p style={{ color: 'var(--subtle)', fontSize: '13.5px', marginBottom: '18px', lineHeight: 1.6 }}>
            输入管理员发放的兑换码即可为当前账户充值配额积分。兑换即刻生效，无需重启网关。
          </p>

          <form onSubmit={handleInlineRedeem}>
            <div style={{ display: 'flex', gap: '8px', marginBottom: '10px' }}>
              <input
                type="text"
                value={inlineCode}
                onChange={(e) => setInlineCode(e.target.value)}
                placeholder="例如: C7-PRO-8892-XK91"
                style={{
                  flex: 1,
                  padding: '9px 12px',
                  border: '1px solid var(--line)',
                  borderRadius: '4px',
                  fontFamily: 'var(--font-mono)',
                  fontSize: '13.5px',
                  textTransform: 'uppercase'
                }}
              />
              <button
                type="submit"
                className="btn-swiss-action"
                disabled={redemption.busy || !inlineCode.trim()}
              >
                {redemption.busy ? '兑换中...' : '立即兑换'}
              </button>
            </div>
            <ErrorNotice error={redemption.error} />
          </form>
        </div>

        <div className="workbench-box bench-bg" style={{ margin: 0 }}>
          <h3 style={{ fontSize: '15px', fontWeight: 800, fontFamily: 'var(--font-display)', marginBottom: '14px', color: 'var(--ink)' }}>
            当前账户套餐与限制
          </h3>
          <dl className="definition-list" style={{ margin: 0, fontFamily: 'var(--font-mono)', fontSize: '13.5px' }}>
            <div><dt>会员等级</dt><dd><strong>PRO LEVEL (活跃使用)</strong></dd></div>
            <div>
              <dt>可用积分余额</dt>
              <dd style={{ color: 'var(--brand)', fontWeight: 700, fontSize: '15px' }}>
                {new Intl.NumberFormat(locale).format(data.balance)} CR
              </dd>
            </div>
            <div><dt>突发限流保护</dt><dd>120 RPM (5 分钟滑动窗口)</dd></div>
            <div><dt>刷新重置周期</dt><dd>每日 00:00 UTC 自动归整</dd></div>
          </dl>
        </div>
      </div>

      {data.enabled && (
        <div style={{ marginTop: '32px' }}>
          <div className="grid-section-header">
            <h2 className="grid-section-title">{t('products')}</h2>
            <span className="grid-section-meta">官方预设算力积分包</span>
          </div>
          {data.quotes.length === 0 ? <EmptyState title="noProducts" /> : (
            <div className="quote-grid" style={{ marginBottom: '32px' }}>
              {data.quotes.map((quote) => (
                <article className="quote-card" key={`${quote.id}:${quote.version}`}>
                  <h3>{quote.label}</h3>
                  <strong>{price(quote.amount_minor, quote.currency, locale)}</strong>
                  <p>{new Intl.NumberFormat(locale).format(quote.units)} CR · {quote.valid_days} {t('validDays').toLowerCase()}</p>
                  <button type="button" className="btn-swiss-action" onClick={() => { setPreview(null); setSelectedQuote(quote); }}>
                    {t('previewOrder')}
                  </button>
                </article>
              ))}
            </div>
          )}
        </div>
      )}
    </>}</Resource>

    <div className="grid-section-header" style={{ marginTop: '24px' }}>
      <h2 className="grid-section-title">{t('orders')}</h2>
      <span className="grid-section-meta">历史充值与兑换审计记录</span>
    </div>

    <Resource source={orders}>{(items) => items.length === 0 ? <EmptyState title="ordersEmpty" icon={<Receipt size={22} />} /> : (
      <div className="bench-table-wrap">
        <table className="bench-table">
          <thead><tr>
            <th>{t('created')}</th>
            <th>{t('amount')}</th>
            <th>{t('credits')}</th>
            <th>{t('status')}</th>
            <th>{t('orderId')}</th>
          </tr></thead>
          <tbody>{items.map((order) => (
            <tr key={order.id}>
              <td><Time value={order.created_at} /></td>
              <td>{price(order.amount_minor, order.currency, locale)}</td>
              <td>{new Intl.NumberFormat(locale).format(order.units)} CR</td>
              <td><StatusChip status={order.status === 'pending' ? 'pendingPayment' : order.status} /></td>
              <td>
                <code style={{ marginRight: '8px' }}>{order.id.slice(0, 12)}</code>
                {order.payment_url && (
                  <a className="btn-swiss-action btn-swiss-secondary" style={{ padding: '3px 8px', fontSize: '11px' }} href={order.payment_url} rel="noopener noreferrer" target="_blank">
                    {t('goToPayment')}
                  </a>
                )}
              </td>
            </tr>
          ))}</tbody>
        </table>
      </div>
    )}</Resource>
    {codeDialog && <Dialog title={t('redeemCode')} onClose={() => { if (!redemption.busy) { setCodeDialog(false); redemption.clear(); } }}>
      <p>{t('redeemHelp')}</p>
      <form onSubmit={(event) => {
        event.preventDefault();
        const input = event.currentTarget.elements.namedItem('code');
        if (!(input instanceof HTMLInputElement)) return;
        const code = input.value.trim();
        input.value = '';
        if (code) void redemption.run(async () => {
          await api('/api/user/billing/redeem', { method: 'POST', body: { code } });
          status.refresh();
          orders.refresh();
          setCodeDialog(false);
        });
      }}>
        <label className="field">{t('redemptionCode')}<input type="password" name="code" required maxLength={240} autoComplete="off" spellCheck={false} autoFocus /></label>
        <ErrorNotice error={redemption.error} />
        <div className="actions modal-actions"><button className="button" type="button" onClick={() => setCodeDialog(false)} disabled={redemption.busy}>{t('cancel')}</button>
          <button className="button button-primary" type="submit" disabled={redemption.busy}>{t(redemption.busy ? 'loading' : 'redeemCode')}</button></div>
      </form>
    </Dialog>}
    {selectedQuote && <QuotePreview quote={selectedQuote} preview={preview} onPreview={setPreview} onClose={() => { if (!created) { setSelectedQuote(null); setPreview(null); } }} />}
    {created && <OrderCreated order={created} onClose={() => { setCreated(null); setSelectedQuote(null); setPreview(null); orders.refresh(); status.refresh(); }} />}
  </>;
}

function QuotePreview({ quote, preview, onPreview, onClose }: {
  quote: BillingStatus['quotes'][number];
  preview: OrderPreview | null;
  onPreview: (preview: OrderPreview | null) => void;
  onClose: () => void;
}) {
  const { locale, t } = useLanguage();
  const mutation = useMutation();
  const [confirming, setConfirming] = useState(false);
  const [created, setCreated] = useState<Order | null>(null);
  const body = { quote_id: quote.id, quote_version: quote.version };
  return <Dialog title={t('previewOrder')} onClose={() => { if (!mutation.busy) onClose(); }}>
    {!preview ? <>
      <p>{t('orderPreviewHelp')}</p>
      <ErrorNotice error={mutation.error} />
      <div className="actions modal-actions"><button className="button" type="button" onClick={onClose}>{t('cancel')}</button>
        <button className="button button-primary" type="button" disabled={mutation.busy} onClick={() => void mutation.run(async () => {
          const value = await api<OrderPreview>('/api/user/orders/preview', { method: 'POST', body });
          onPreview(value);
        })}>{t(mutation.busy ? 'loading' : 'previewOrder')}</button></div>
    </> : <>
      <dl className="definition-list">
        <div><dt>{t('products')}</dt><dd>{preview.quote.label}</dd></div>
        <div><dt>{t('amount')}</dt><dd>{price(preview.quote.amount_minor, preview.quote.currency, locale)}</dd></div>
        <div><dt>{t('credits')}</dt><dd>{new Intl.NumberFormat(locale).format(preview.quote.units)}</dd></div>
        <div><dt>{t('validDays')}</dt><dd>{preview.quote.valid_days}</dd></div>
        <div><dt>{t('creditBalance')}</dt><dd>{new Intl.NumberFormat(locale).format(preview.balance)}</dd></div>
        <div><dt>{t('projectedBalance')}</dt><dd>{new Intl.NumberFormat(locale).format(preview.projected_balance)}</dd></div>
      </dl>
      <p>{t('orderPreviewHelp')}</p>
      <ErrorNotice error={mutation.error} />
      <div className="actions modal-actions"><button className="button" type="button" disabled={mutation.busy} onClick={onClose}>{t('cancel')}</button>
        <button className="button button-primary" type="button" disabled={mutation.busy} onClick={() => setConfirming(true)}>{t('createOrder')}</button></div>
      {confirming && <Confirm title="confirmOrder" help="confirmOrderHelp" yes="createOrder" onClose={() => setConfirming(false)} onConfirm={async () => {
        const order = await api<Order>('/api/user/orders', { method: 'POST', body });
        setCreated(order);
        setConfirming(false);
      }} />}
      {created && <OrderCreated order={created} onClose={() => { setCreated(null); onClose(); }} />}
    </>}
  </Dialog>;
}

function OrderCreated({ order, onClose }: { order: Order; onClose: () => void }) {
  const { t, locale } = useLanguage();
  return <Dialog title={t('orderCreated')} onClose={onClose}>
    <dl className="definition-list">
      <div><dt>{t('orderId')}</dt><dd><code>{order.id}</code></dd></div>
      <div><dt>{t('status')}</dt><dd><StatusChip status={order.status === 'pending' ? 'pendingPayment' : order.status} /></dd></div>
      <div><dt>{t('amount')}</dt><dd>{price(order.amount_minor, order.currency, locale)}</dd></div>
      <div><dt>{t('credits')}</dt><dd>{new Intl.NumberFormat(locale).format(order.units)}</dd></div>
      <div><dt>{t('expiration')}</dt><dd><Time value={order.expires_at} /></dd></div>
    </dl>
    {order.payment_url && <div className="actions modal-actions"><a className="button button-primary" href={order.payment_url} target="_blank" rel="noopener noreferrer">{t('goToPayment')}</a></div>}
    {!order.payment_url && <p>{t('paymentRedirectHelp')}</p>}
    <div className="actions modal-actions"><button className="button" type="button" onClick={onClose}>{t('close')}</button></div>
  </Dialog>;
}
