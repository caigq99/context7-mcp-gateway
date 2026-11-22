import { CreditCard, Receipt, TicketCheck } from 'lucide-react';
import { useState } from 'react';
import { api } from './api';
import type { BillingStatus, Order, OrderPreview } from './api';
import { Confirm, Dialog, EmptyState, ErrorNotice, PageHeading, Panel, Resource, StatusChip, Time, useMutation, useResource } from './components';
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
  const [selectedQuote, setSelectedQuote] = useState<BillingStatus['quotes'][number] | null>(null);
  const [preview, setPreview] = useState<OrderPreview | null>(null);
  const [created, setCreated] = useState<Order | null>(null);
  return <>
    <PageHeading title="billing" help="billingHelp" actions={<button className="button" type="button" onClick={() => setCodeDialog(true)}><TicketCheck size={16} aria-hidden="true" />{t('redeemCode')}</button>} />
    <Resource source={status}>{(data) => <>
      {!data.enabled && <section className="notice warning-notice" role="status"><CreditCard size={18} aria-hidden="true" /><div><strong>{t('billingDisabled')}</strong><p>{t('billingDisabledHelp')}</p></div></section>}
      <div className="stats-grid">
        <article className="stat-card"><span className="stat-label">{t('creditBalance')}</span><strong>{new Intl.NumberFormat(locale).format(data.balance)}</strong><span className="stat-caption">{t('billingCreditHelp')}</span></article>
      </div>
      {data.enabled && <Panel title={t('products')}>
        {data.quotes.length === 0 ? <EmptyState title="noProducts" /> : <div className="quote-grid">{data.quotes.map((quote) => <article className="quote-card" key={`${quote.id}:${quote.version}`}>
          <h3>{quote.label}</h3><strong>{price(quote.amount_minor, quote.currency, locale)}</strong>
          <p>{new Intl.NumberFormat(locale).format(quote.units)} · {quote.valid_days} {t('validDays').toLowerCase()}</p>
          <button type="button" className="button button-primary" onClick={() => { setPreview(null); setSelectedQuote(quote); }}>{t('previewOrder')}</button>
        </article>)}</div>}
      </Panel>}
    </>}</Resource>
    <Panel title={t('orders')}><Resource source={orders}>{(items) => items.length === 0 ? <EmptyState title="ordersEmpty" icon={<Receipt size={22} />} /> :
      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">{t('created')}</th><th scope="col">{t('amount')}</th><th scope="col">{t('credits')}</th><th scope="col">{t('status')}</th><th scope="col">{t('orderId')}</th></tr></thead>
        <tbody>{items.map((order) => <tr key={order.id}>
          <td><Time value={order.created_at} /></td><td>{price(order.amount_minor, order.currency, locale)}</td><td>{new Intl.NumberFormat(locale).format(order.units)}</td>
          <td><StatusChip status={order.status === 'pending' ? 'pendingPayment' : order.status} /></td><td><code>{order.id.slice(0, 12)}</code>{order.payment_url && <a className="button button-sm" href={order.payment_url} rel="noopener noreferrer" target="_blank">{t('goToPayment')}</a>}</td>
        </tr>)}</tbody>
      </table></div>}
    </Resource></Panel>
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
