import { AlertCircle, Check, CircleHelp, Copy, RefreshCw, X } from 'lucide-react';
import { useEffect, useId, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { ApiError, api } from './api';
import { useLanguage } from './i18n';
import type { Text } from './i18n';

export type ResourceState<T> =
  | { state: 'loading' }
  | { state: 'ready'; data: T }
  | { state: 'error'; error: unknown };

export function useResource<T>(path: string | null) {
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{ path: string; revision: number; value: ResourceState<T> }>();
  useEffect(() => {
    if (!path) return;
    const controller = new AbortController();
    api<T>(path, { signal: controller.signal }).then(
      (data) => {
        if (!controller.signal.aborted) setResult({ path, revision, value: { state: 'ready', data } });
      },
      (error: unknown) => {
        if (!controller.signal.aborted) setResult({ path, revision, value: { state: 'error', error } });
      },
    );
    return () => controller.abort();
  }, [path, revision]);
  const value: ResourceState<T> = result?.path === path && result.revision === revision
    ? result.value
    : { state: 'loading' };
  return { value, refresh: () => setRevision((version) => version + 1) };
}

export function useMutation() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const running = useRef(false);
  async function run(task: () => Promise<void>) {
    if (running.current) return false;
    running.current = true;
    setBusy(true);
    setError(undefined);
    try {
      await task();
      return true;
    } catch (error) {
      setError(error);
      return false;
    } finally {
      setBusy(false);
      running.current = false;
    }
  }
  return { busy, error, run, clear: () => setError(undefined) };
}

const errors: Partial<Record<string, Text>> = {
  unauthorized: 'signInRequired',
  invalid_gateway_key: 'unauthorizedKey',
  not_found: 'notFound',
  forbidden: 'forbidden',
  conflict: 'conflict',
  not_configured: 'notConfigured',
  credential_unavailable: 'noUpstream',
  invalid_request: 'invalid',
  upstream_unavailable: 'upstreamUnavailable',
  upstream_rate_limited: 'upstreamRateLimited',
  pool_unavailable: 'noUpstream',
  quota_exceeded: 'quotaExceeded',
  local_quota_exceeded: 'quotaExceeded',
  local_rate_limited: 'rateLimited',
  rate_limited: 'rateLimited',
  passkey_failed: 'passkeyFailed',
};

export function ErrorNotice({ error, retry }: { error: unknown; retry?: () => void }) {
  const { t } = useLanguage();
  if (!error) return null;
  const requestId = error instanceof ApiError ? error.requestId : null;
  const fallback = error instanceof ApiError
    ? ({ 400: 'invalid', 401: 'signInRequired', 403: 'forbidden', 404: 'notFound', 422: 'invalid', 429: 'rateLimited', 503: 'notConfigured' } as Partial<Record<number, Text>>)[error.status] ?? 'error'
    : 'error';
  const text = error instanceof ApiError ? errors[error.code] ?? fallback : fallback;
  return (
    <div className="notice error-notice" role="alert">
      <AlertCircle size={18} aria-hidden="true" />
      <div>
        <div>{t(text)}</div>
        {requestId && <small>{t('requestId')} <code>{requestId}</code></small>}
      </div>
      {retry && <button type="button" className="button button-quiet" onClick={retry}>{t('retry')}</button>}
    </div>
  );
}

export function Resource<T>({ source, children }: {
  source: ReturnType<typeof useResource<T>>;
  children: (data: T) => ReactNode;
}) {
  if (source.value.state === 'loading') return <Loading />;
  if (source.value.state === 'error') return <ErrorNotice error={source.value.error} retry={source.refresh} />;
  return children(source.value.data);
}

export function Loading() {
  const { t } = useLanguage();
  return <div className="loading-state" role="status"><RefreshCw size={17} className="spinner" aria-hidden="true" />{t('loading')}</div>;
}

export function PageHeading({ title, help, actions }: { title: Text; help?: Text; actions?: ReactNode }) {
  const { t } = useLanguage();
  return (
    <header className="page-heading">
      <div>
        <h1 id="page-title">{t(title)}</h1>
        {help && <p>{t(help)}</p>}
      </div>
      {actions && <div className="actions">{actions}</div>}
    </header>
  );
}

export function Panel({ title, hint, actions, children }: {
  title?: ReactNode;
  hint?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="panel">
      {(title || hint || actions) && <div className="panel-header">
        <div>
          {title && <h2>{title}</h2>}
          {hint && <p>{hint}</p>}
        </div>
        {actions && <div className="actions">{actions}</div>}
      </div>}
      {children}
    </section>
  );
}

export function EmptyState({ title, hint, icon }: { title: Text; hint?: Text; icon?: ReactNode }) {
  const { t } = useLanguage();
  return (
    <div className="empty-state">
      <span className="empty-icon" aria-hidden="true">{icon ?? <CircleHelp size={22} />}</span>
      <strong>{t(title)}</strong>
      {hint && <p>{t(hint)}</p>}
    </div>
  );
}

export function Dialog({ title, onClose, children }: { title: string; onClose: () => void; children: ReactNode }) {
  const { t } = useLanguage();
  const label = useId();
  const dialog = useRef<HTMLDialogElement | null>(null);
  useEffect(() => {
    const current = dialog.current;
    current?.showModal();
    return () => current?.close();
  }, []);
  return (
    <dialog
      className="modal"
      aria-labelledby={label}
      ref={dialog}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
    >
      <header className="modal-header">
        <h2 id={label}>{title}</h2>
        <button type="button" className="button icon-button button-quiet" onClick={onClose} aria-label={t('close')}>
          <X size={20} aria-hidden="true" />
        </button>
      </header>
      {children}
    </dialog>
  );
}

export function Confirm({ title, help, yes, onClose, onConfirm }: {
  title: Text;
  help: Text;
  yes: Text;
  onClose: () => void;
  onConfirm: () => Promise<unknown>;
}) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return (
    <Dialog title={t(title)} onClose={() => { if (!mutation.busy) onClose(); }}>
      <p>{t(help)}</p>
      <ErrorNotice error={mutation.error} />
      <div className="actions modal-actions">
        <button type="button" className="button" disabled={mutation.busy} onClick={onClose}>{t('cancel')}</button>
        <button type="button" className="button button-danger" disabled={mutation.busy} onClick={() => void mutation.run(async () => {
          await onConfirm();
          onClose();
        })}>{t(mutation.busy ? 'loading' : yes)}</button>
      </div>
    </Dialog>
  );
}

export function CopyButton({ text }: { text: string }) {
  const { t } = useLanguage();
  const [state, setState] = useState<'ready' | 'copied' | 'failed'>('ready');
  const [previousText, setPreviousText] = useState(text);
  if (previousText !== text) {
    setPreviousText(text);
    setState('ready');
  }
  useEffect(() => {
    if (state === 'ready') return;
    const id = window.setTimeout(() => setState('ready'), 3000);
    return () => window.clearTimeout(id);
  }, [state]);
  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      setState('copied');
    } catch {
      setState('failed');
    }
  }
  return (
    <div className="copy-control">
      <button type="button" className="button button-sm" onClick={() => void copy()}>
        {state === 'copied' ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        {t(state === 'copied' ? 'copied' : 'copy')}
      </button>
      {state === 'failed' && <small role="status">{t('copyFailed')}</small>}
    </div>
  );
}

export function CopyBlock({ text, label }: { text: string; label?: string }) {
  return (
    <div className="code-block">
      <div className="code-header">
        <span>{label}</span>
        <CopyButton text={text} />
      </div>
      <pre tabIndex={0}><code>{text}</code></pre>
    </div>
  );
}

const statusStyles: Partial<Record<string, { label: Text; tone: string }>> = {
  available: { label: 'available', tone: 'good' },
  unavailable: { label: 'unavailable', tone: 'bad' },
  success: { label: 'conditionSuccess', tone: 'good' },
  failure: { label: 'conditionFailed', tone: 'bad' },
  active: { label: 'active', tone: 'good' },
  enabled: { label: 'enabled', tone: 'good' },
  disabled: { label: 'disabled', tone: 'quiet' },
  pending: { label: 'pending', tone: 'warning' },
  pendingPayment: { label: 'pendingPayment', tone: 'warning' },
  unsupported: { label: 'unavailable', tone: 'quiet' },
  upstream_error: { label: 'upstreamUnavailable', tone: 'bad' },
  invalid: { label: 'invalidKey', tone: 'bad' },
  permission_error: { label: 'permissionError', tone: 'bad' },
  rate_limited: { label: 'limited', tone: 'warning' },
  revoked: { label: 'revoked', tone: 'quiet' },
  expired: { label: 'expired', tone: 'quiet' },
  scheduled: { label: 'scheduled', tone: 'warning' },
  ended: { label: 'passed', tone: 'quiet' },
  settled: { label: 'settled', tone: 'good' },
  cancelled: { label: 'cancelled', tone: 'quiet' },
  payment_pending: { label: 'pendingPayment', tone: 'warning' },
  failed: { label: 'conditionFailed', tone: 'bad' },
  grant: { label: 'ledgerGrant', tone: 'good' },
  consume: { label: 'ledgerConsume', tone: 'quiet' },
  reverse: { label: 'ledgerReverse', tone: 'warning' },
  manual: { label: 'ledgerManual', tone: 'warning' },
};

export function StatusChip({ status }: { status: string }) {
  const { t } = useLanguage();
  const display = statusStyles[status];
  return <span className={`status-chip ${display?.tone ?? 'quiet'}`}><i aria-hidden="true" />{display ? t(display.label) : status}</span>;
}

export function Time({ value, dateOnly = false, utc = false }: { value: string | number | null | undefined; dateOnly?: boolean; utc?: boolean }) {
  const { locale, t } = useLanguage();
  if (value === null || value === undefined || value === '') return <span className="muted">{t('never')}</span>;
  const parsed = new Date(typeof value === 'number' ? value * 1000 : value);
  if (Number.isNaN(parsed.getTime())) return <span className="muted">{t('unavailable')}</span>;
  const formatted = new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium',
    ...(dateOnly ? {} : { timeStyle: 'short' as const }),
    ...(utc ? { timeZone: 'UTC' } : {}),
  }).format(parsed);
  return <time dateTime={parsed.toISOString()}>{formatted}</time>;
}
