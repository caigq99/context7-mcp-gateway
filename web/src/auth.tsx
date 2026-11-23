import { ArrowUpRight, Fingerprint, KeyRound, Layers3, ShieldCheck } from 'lucide-react';
import { browserSupportsWebAuthn, startAuthentication, startRegistration } from '@simplewebauthn/browser';
import type { PublicKeyCredentialCreationOptionsJSON, PublicKeyCredentialRequestOptionsJSON } from '@simplewebauthn/browser';
import { useState } from 'react';
import { api, ApiError } from './api';
import { Confirm, Dialog, EmptyState, ErrorNotice, Loading, Panel, Resource, Time, useMutation, useResource } from './components';
import { useLanguage } from './i18n';
import { Link } from './navigation';

export type AdminSession = {
  authenticated: boolean;
  password_login_available: boolean;
  passkey_available: boolean;
  passkey_login_available: boolean;
};
type WebAuthnOptions<TOptions> = { publicKey: TOptions };
const authPath = '/api/admin/auth';

export function UserSignIn() {
  const { t } = useLanguage();
  const features = [
    { title: 'signInFeature', help: 'signInFeatureHelp', Icon: KeyRound },
    { title: 'quotaFeature', help: 'quotaFeatureHelp', Icon: Layers3 },
    { title: 'setupFeature', help: 'setupFeatureHelp', Icon: ShieldCheck },
  ] as const;
  return (
    <div className="sign-in-grid">
      <section className="sign-in-card">
        <h1 id="page-title">{t('userConsole')}</h1>
        <p>{t('intro')}</p>
        <a className="button button-primary" href="/auth/linuxdo">{t('signIn')}<ArrowUpRight size={16} aria-hidden="true" /></a>
        <p className="sign-in-note">{t('browserSignIn')}</p>
      </section>
      <div className="sign-in-feature-list">
        {features.map(({ title, help, Icon }) => <article key={title}>
          <span className="feature-icon"><Icon size={18} aria-hidden="true" /></span>
          <div><h2>{t(title)}</h2><p>{t(help)}</p></div>
        </article>)}
      </div>
    </div>
  );
}

export function AdminSignIn({ session, onSuccess }: { session: AdminSession | null; onSuccess: () => void }) {
  const { t } = useLanguage();
  const passwordMutation = useMutation();
  const passkeyMutation = useMutation();
  const [register, setRegister] = useState(false);
  if (!session) return <div className="sign-in-grid admin-sign-in"><section className="sign-in-card"><Loading /></section></div>;
  const supported = window.isSecureContext && browserSupportsWebAuthn();
  const passwordAvailable = session.password_login_available;
  const passkeyLoginAvailable = session.passkey_login_available && supported;
  // Passwordless gateways can still bootstrap their first passkey from this page.
  const enrollmentAvailable = !passwordAvailable && !session.passkey_login_available && session.passkey_available && supported;
  const noMethods = !passwordAvailable && !passkeyLoginAvailable && !enrollmentAvailable;
  return (
    <div className="sign-in-grid admin-sign-in">
      <section className="sign-in-card">
        <span className="feature-icon"><KeyRound size={22} aria-hidden="true" /></span>
        <h1 id="page-title">{t('adminAccess')}</h1>
        {passwordAvailable && <p>{t('passwordSignInHelp')}</p>}
        {!passwordAvailable && passkeyLoginAvailable && <p>{t('passkeyHelp')}</p>}
        {enrollmentAvailable && <p>{t('enrollmentHelp')}</p>}
        {noMethods && <p role="status">{t(session.passkey_available ? 'passkeyUnavailable' : 'notConfigured')}</p>}
        {passwordAvailable && <form className="password-form" onSubmit={(event) => {
          event.preventDefault();
          const fields = new FormData(event.currentTarget);
          const username = String(fields.get('username') ?? '').trim();
          const password = String(fields.get('password') ?? '');
          if (!username || !password) return;
          void passwordMutation.run(async () => {
            await api<AdminSession>(`${authPath}/password/login`, { method: 'POST', body: { username, password } });
            onSuccess();
          });
        }}>
          <label className="field">{t('adminUsername')}
            <input name="username" autoComplete="username" autoCapitalize="none" spellCheck={false} autoFocus required maxLength={320} />
          </label>
          <label className="field">{t('adminPassword')}
            <input type="password" name="password" autoComplete="current-password" required maxLength={1024} />
          </label>
          <ErrorNotice error={passwordMutation.error} />
          <button className="button button-primary password-submit" type="submit" disabled={passwordMutation.busy}>
            <KeyRound size={17} aria-hidden="true" />{t(passwordMutation.busy ? 'loading' : 'adminSignIn')}
          </button>
        </form>}
        {passkeyLoginAvailable && <div className={passwordAvailable ? 'passkey-alternative' : undefined}>
          {passwordAvailable && <div className="sign-in-divider" aria-hidden="true">{t('signInOr')}</div>}
          <button className={`button${passwordAvailable ? '' : ' button-primary'}`} type="button" disabled={passkeyMutation.busy}
            onClick={() => void passkeyMutation.run(async () => {
              const start = await api<WebAuthnOptions<PublicKeyCredentialRequestOptionsJSON>>(`${authPath}/login/start`, { method: 'POST' });
              const credential = await performPasskey(() => startAuthentication({ optionsJSON: start.publicKey }));
              await api<AdminSession>(`${authPath}/login/finish`, { method: 'POST', body: { credential } });
              onSuccess();
            })}>
            <Fingerprint size={17} aria-hidden="true" />{t(passkeyMutation.busy ? 'loading' : 'passkeySignIn')}
          </button>
          <p className="sign-in-note">{t('passkeySignInHelp')}</p>
          <ErrorNotice error={passkeyMutation.error} />
        </div>}
        {enrollmentAvailable && <div className="passkey-alternative">
          <button className="button button-primary" type="button" onClick={() => setRegister(true)}><Fingerprint size={17} aria-hidden="true" />{t('enroll')}</button>
        </div>}
        <Link className="text-link" href="/console/dashboard">{t('userConsole')}</Link>
      </section>
      {register && <RegisterPasskey requireCode onClose={() => setRegister(false)} onSuccess={onSuccess} />}
    </div>
  );
}

async function performPasskey<T>(perform: () => Promise<T>) {
  try {
    return await perform();
  } catch (error) {
    console.error('WebAuthn ceremony failed', error);
    throw new ApiError(400, 'passkey_failed', null);
  }
}

export function RegisterPasskey({ requireCode = false, onClose, onSuccess }: {
  requireCode?: boolean;
  onClose: () => void;
  onSuccess: () => void;
}) {
  const { t } = useLanguage();
  const mutation = useMutation();
  return <Dialog title={t('enroll')} onClose={() => { if (!mutation.busy) onClose(); }}>
    {requireCode && <p>{t('enrollmentHelp')}</p>}
    <form onSubmit={(event) => {
      event.preventDefault();
      const body = new FormData(event.currentTarget);
      const name = String(body.get('name')).trim();
      const enrollment_code = requireCode ? String(body.get('enrollment_code')).trim() : undefined;
      if (!name || (requireCode && !enrollment_code)) return;
      // Drop the typed code from the form before opening the authenticator prompt.
      const codeInput = event.currentTarget.elements.namedItem('enrollment_code');
      if (codeInput instanceof HTMLInputElement) codeInput.value = '';
      void mutation.run(async () => {
        const start = await api<WebAuthnOptions<PublicKeyCredentialCreationOptionsJSON>>(`${authPath}/register/start`, {
          method: 'POST', body: { name, enrollment_code },
        });
        const credential = await performPasskey(() => startRegistration({ optionsJSON: start.publicKey }));
        await api<AdminSession>(`${authPath}/register/finish`, { method: 'POST', body: { credential } });
        onClose();
        onSuccess();
      });
    }}>
      <label className="field">{t('securityKeyName')}<input name="name" required maxLength={120} autoFocus /></label>
      {requireCode && <label className="field">{t('enrollmentCode')}
        <input type="password" name="enrollment_code" required maxLength={240} autoComplete="off" spellCheck={false} />
      </label>}
      <ErrorNotice error={mutation.error} />
      <div className="actions modal-actions">
        <button className="button" type="button" onClick={onClose} disabled={mutation.busy}>{t('cancel')}</button>
        <button className="button button-primary" type="submit" disabled={mutation.busy}>{t(mutation.busy ? 'loading' : 'enroll')}</button>
      </div>
    </form>
  </Dialog>;
}

type Passkey = { id: string; name: string; created_at: number; last_used_at: number | null };

export function PasskeySettings() {
  const { t } = useLanguage();
  const path = `${authPath}/passkeys`;
  const passkeys = useResource<Passkey[]>(path);
  const session = useResource<AdminSession>(`${authPath}/session`);
  const [register, setRegister] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const [registered, setRegistered] = useState(false);
  const passwordFallback = session.value.state === 'ready' && session.value.data.password_login_available;
  return <Panel title={t('passkeys')} hint={t('passkeysHelp')} actions={
    <button type="button" className="button" onClick={() => setRegister(true)} disabled={!window.isSecureContext || !browserSupportsWebAuthn()}>
      <Fingerprint size={16} aria-hidden="true" />{t('enroll')}
    </button>
  }>
    {registered && <div className="notice success-notice" role="status">{t('passkeyRegistered')}</div>}
    <Resource source={passkeys}>{(keys) => keys.length === 0 ? <EmptyState title="noPasskeys" /> :
      <div className="table-scroll"><table className="data-table">
        <thead><tr><th scope="col">{t('securityKeyName')}</th><th scope="col">{t('created')}</th><th scope="col">{t('lastUsed')}</th><th scope="col"><span className="sr-only">{t('manageTokens')}</span></th></tr></thead>
        <tbody>{keys.map((key) => <tr key={key.id}>
          <td><strong>{key.name}</strong></td><td><Time value={key.created_at} /></td><td><Time value={key.last_used_at} /></td>
          <td><button className="button button-sm button-text-danger" type="button" disabled={!passwordFallback && keys.length <= 1}
            onClick={() => setRemoving(key.id)}>{t('remove')}</button></td>
        </tr>)}</tbody>
      </table></div>}
    </Resource>
    {register && <RegisterPasskey onClose={() => setRegister(false)} onSuccess={() => { setRegistered(true); passkeys.refresh(); }} />}
    {removing && <Confirm title="removePasskey" help="removePasskeyHelp" yes="remove" onClose={() => setRemoving(null)} onConfirm={async () => {
      await api<void>(`${path}/${encodeURIComponent(removing)}`, { method: 'DELETE' });
      passkeys.refresh();
    }} />}
  </Panel>;
}
