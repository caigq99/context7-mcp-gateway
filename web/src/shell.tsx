import { Activity, ArrowLeft, BookOpenText, ChevronDown, CircleDollarSign, Command, Database, Languages, LayoutDashboard, LogOut, Menu, Megaphone, Settings, Shield, UsersRound, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { AdminBilling, AnnouncementsPage, PoolPage, SettingsPage, UserManagement } from './admin-pages';
import { AdminSignIn, UserSignIn } from './auth';
import type { AdminSession } from './auth';
import { api, ApiError } from './api';
import type { User } from './api';
import { ErrorNotice, Loading, useMutation, useResource } from './components';
import { useLanguage } from './i18n';
import type { Locale, Text } from './i18n';
import { Link, NavigationProvider, useNavigation } from './navigation';
import { InitialTokenNotice, TokenPage } from './token-page';
import { BillingPage } from './billing-page';
import { Dashboard, RequestPage } from './usage-pages';
import { SetupPage } from './setup-page';

type NavItem = { path: string; label: Text; Icon: typeof LayoutDashboard };

const userNavigation: NavItem[] = [
  { path: '/console/dashboard', label: 'dashboard', Icon: LayoutDashboard },
  { path: '/console/tokens', label: 'tokens', Icon: Command },
  { path: '/console/requests', label: 'requests', Icon: Activity },
  { path: '/console/setup', label: 'setup', Icon: BookOpenText },
  { path: '/console/billing', label: 'billing', Icon: CircleDollarSign },
];

const adminNavigation: NavItem[] = [
  { path: '/admin/dashboard', label: 'dashboard', Icon: LayoutDashboard },
  { path: '/admin/pools', label: 'pools', Icon: Database },
  { path: '/admin/users', label: 'users', Icon: UsersRound },
  { path: '/admin/logs', label: 'requests', Icon: Activity },
  { path: '/admin/announcements', label: 'announcements', Icon: Megaphone },
  { path: '/admin/settings', label: 'settings', Icon: Settings },
  { path: '/admin/billing', label: 'billingCodes', Icon: CircleDollarSign },
];

const routeLabels: Record<string, Text> = {
  dashboard: 'dashboard', tokens: 'tokens', requests: 'requests', setup: 'setup', billing: 'billing',
  pools: 'pools', users: 'users', logs: 'requests', announcements: 'announcements', settings: 'settings',
};

export function App() {
  return <NavigationProvider><Application /></NavigationProvider>;
}

function Application() {
  const { location, navigate } = useNavigation();
  const { t, locale, setLocale } = useLanguage();
  const admin = location.pathname.startsWith('/admin');
  const user = useResource<{ user: User }>(admin ? null : '/api/user/me');
  const adminSession = useResource<AdminSession>(admin ? '/api/admin/auth/session' : null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [userMenuOpen, setUserMenuOpen] = useState(false);
  const signOut = useMutation();
  const currentUser = user.value.state === 'ready' ? user.value.data.user : null;
  const loggedIn = Boolean(currentUser && currentUser.status === 'active');
  const isAdmin = adminSession.value.state === 'ready' && adminSession.value.data.authenticated;
  const exactConsole = /^\/(?:console|admin)\/?$/.test(location.pathname);
  useEffect(() => {
    if (exactConsole) navigate(admin ? '/admin/dashboard' : '/console/dashboard');
    setMenuOpen(false);
    setUserMenuOpen(false);
  }, [location.pathname, exactConsole, admin, navigate]);

  const [base, resource] = useMemo(() => routeFrom(location.pathname), [location.pathname]);
  const notFound = resource === null;
  const protectedBySession = admin ? isAdmin : loggedIn;
  const authLoading = admin ? adminSession.value.state === 'loading' : user.value.state === 'loading';
  const authError = admin ? adminSession.value.state === 'error' ? adminSession.value.error : null :
    user.value.state === 'error' && !(user.value.error instanceof ApiError && user.value.error.status === 401) ? user.value.error : null;
  const signOutAction = async () => {
    await api<void>(admin ? '/api/admin/auth/logout' : '/api/user/logout', { method: 'POST' });
    if (admin) adminSession.refresh(); else user.refresh();
    navigate(admin ? '/admin' : '/console');
  };

  if (admin && resource === 'login') {
    if (adminSession.value.state === 'loading') return <><SkipToContent /><main className="auth-loading"><Loading /></main></>;
    if (adminSession.value.state === 'error') return <><SkipToContent /><main className="auth-loading"><ErrorNotice error={adminSession.value.error} retry={adminSession.refresh} /></main></>;
    return <><SkipToContent /><div className="login-page"><AdminSignIn session={adminSession.value.data} onSuccess={() => {
      adminSession.refresh();
      navigate('/admin/dashboard');
    }} /></div></>;
  }

  if (!admin && authLoading) return <><SkipToContent /><main className="auth-loading"><Loading /></main></>;

  if (authError && resource !== 'login') return <><SkipToContent /><main className="auth-loading"><ErrorNotice error={authError} retry={admin ? adminSession.refresh : user.refresh} /></main></>;

  if (admin && adminSession.value.state === 'error' && !isAdmin) return <>
    <SkipToContent />
    <Standalone title={t('adminAccess')} onBack={() => navigate('/console/dashboard')}>
      <ErrorNotice error={adminSession.value.error} retry={adminSession.refresh} />
      <a className="button button-primary" href="/admin/login">{t('adminSignIn')}</a>
    </Standalone>
  </>;

  if (!admin && !protectedBySession) return <><SkipToContent /><div className="login-page"><main className="user-login-layout" id="main-content"><div className="standalone-brand"><span className="brand-mark"><BookOpenText size={20} aria-hidden="true" /></span><strong>{t('product')}</strong></div><UserSignIn />
      <div className="sign-in-links"><Link href="/admin/login">{t('adminAccess')}</Link><LanguageToggle locale={locale} setLocale={setLocale} /></div>
  </main></div></>;

  if (admin && !protectedBySession) return <>
    <SkipToContent />
    <div className="login-page"><AdminSignIn session={adminSession.value.state === 'ready' ? adminSession.value.data : null} onSuccess={() => {
      adminSession.refresh();
      navigate('/admin/dashboard');
    }} /></div>
  </>;

  return <>
    <SkipToContent />
    <div className={`app-shell${admin ? ' is-admin' : ''}`}>
      {menuOpen && <button className="mobile-backdrop" aria-label={t('close')} onClick={() => setMenuOpen(false)} />}
      <aside className={`sidebar${menuOpen ? ' sidebar-open' : ''}`} aria-label={t('navLabel')}>
        <Link className="brand" href={admin ? '/admin/dashboard' : '/console/dashboard'}>
          <span className="brand-mark"><BookOpenText size={19} aria-hidden="true" /></span>
          <span className="brand-type"><strong>Context7</strong><small>GATEWAY</small></span>
          <button className="mobile-close" aria-label={t('close')} onClick={(event) => { event.preventDefault(); setMenuOpen(false); }}><X size={18} /></button>
        </Link>
        <div className="workspace-label">{t(admin ? 'adminConsole' : 'workspace')}</div>
        <nav className="nav-stack" aria-label={t('navLabel')}>
          <span className="nav-group-label">{t(admin ? 'admin' : 'navSectionUse')}</span>
          {(admin ? adminNavigation : userNavigation).map(({ path, label, Icon }) => <NavLink key={path} path={path} label={label} Icon={Icon} />)}
        </nav>
        <div className="sidebar-spacer" />
        <div className="sidebar-footer">
          <Link href={admin ? '/console/dashboard' : '/admin'} className="switch-console"><Shield size={16} aria-hidden="true" />{t(admin ? 'userConsole' : 'adminConsole')}</Link>
          <button className="locale-button" type="button" onClick={() => setLocale(locale === 'zh-CN' ? 'en' : 'zh-CN')}><Languages size={16} aria-hidden="true" />{locale === 'zh-CN' ? '简体中文' : 'English'}<ChevronDown size={14} aria-hidden="true" /></button>
          <div className="profile-card">
            <span className="avatar">{(admin ? 'A' : currentUser?.display_name.slice(0, 1) ?? 'G').toLocaleUpperCase()}</span>
            <div className="profile-info"><strong>{admin ? t('adminConsole') : currentUser?.display_name}</strong><small>{admin ? t('passkeys') : t('userConsole')}</small></div>
            <button className="profile-action" type="button" aria-label={t('signOut')} disabled={signOut.busy} onClick={() => void signOut.run(signOutAction)}><LogOut size={16} aria-hidden="true" /></button>
          </div>
        </div>
      </aside>
      <main className="main-content" id="main-content">
        <header className="topbar">
          <div className="topbar-left"><button className="mobile-menu" type="button" aria-label={t('smallScreenMenu')} aria-expanded={menuOpen} onClick={() => setMenuOpen(true)}><Menu size={20} aria-hidden="true" /></button>
            <nav className="breadcrumb" aria-label="Breadcrumb"><Link href={admin ? '/admin/dashboard' : '/console/dashboard'}>{t(admin ? 'adminConsole' : 'workspace')}</Link><span aria-hidden="true">/</span><strong>{resource ? t(routeLabels[base] ?? (admin ? 'adminConsole' : 'dashboard')) : t('unknownRoute')}</strong></nav>
          </div>
          <div className="topbar-actions"><span className="environment-pill"><i aria-hidden="true" />{t('gateway')}</span>
            <button className="avatar small" aria-label={currentUser?.display_name ?? t('adminConsole')} onClick={() => setUserMenuOpen((value) => !value)}>{(admin ? 'A' : currentUser?.display_name.slice(0, 1) ?? 'G').toLocaleUpperCase()}</button>
          </div>
          {userMenuOpen && <div className="user-menu" role="menu"><button role="menuitem" type="button" onClick={() => { setLocale(locale === 'zh-CN' ? 'en' : 'zh-CN'); setUserMenuOpen(false); }}><Languages size={15} />{t('language')}</button>
            <button role="menuitem" type="button" onClick={() => { setUserMenuOpen(false); void signOut.run(signOutAction); }}><LogOut size={15} />{t('signOut')}</button></div>}
        </header>
        <div className="page-content">
          {!admin && <InitialTokenNotice />}
          <ErrorNotice error={signOut.error} />
          {notFound ? <section className="page-heading"><h1 id="page-title">{t('unknownRoute')}</h1><Link className="button button-primary" href={admin ? '/admin/dashboard' : '/console/dashboard'}><ArrowLeft size={16} aria-hidden="true" />{t('home')}</Link></section>
            : <RoutePage admin={admin} base={base} resource={resource} />}
          <footer className="app-footer"><span>Context7 Gateway</span><span>·</span><span>{t('workspace')}</span></footer>
        </div>
      </main>
    </div>
  </>;
}

function SkipToContent() {
  const { t } = useLanguage();
  return <a className="skip-link" href="#main-content">{t('skip')}</a>;
}

function Standalone({ children, title, onBack, hideBack = false }: { children: React.ReactNode; title: string; onBack: () => void; hideBack?: boolean }) {
  const { t } = useLanguage();
  return <main className="standalone" id="main-content">
    <div className="standalone-brand"><span className="brand-mark"><BookOpenText size={20} aria-hidden="true" /></span><strong>{t('product')}</strong></div>
    <section className="sign-in-card"><h1 id="page-title">{title}</h1>{children}
      {!hideBack && <button className="button button-quiet" type="button" onClick={onBack}>{t('home')}</button>}
    </section>
  </main>;
}

function LanguageToggle({ locale, setLocale }: { locale: Locale; setLocale: (locale: Locale) => void }) {
  const { t } = useLanguage();
  return <button className="locale-button inline-locale" type="button" onClick={() => setLocale(locale === 'zh-CN' ? 'en' : 'zh-CN')}><Languages size={16} aria-hidden="true" />{t('language')}</button>;
}

function NavLink({ path, label, Icon }: NavItem) {
  const { location } = useNavigation();
  const { t } = useLanguage();
  const active = location.pathname === path || location.pathname.startsWith(path + '/');
  return <Link className={`nav-link${active ? ' active' : ''}`} href={path} aria-current={active ? 'page' : undefined}>
    <Icon size={17} strokeWidth={1.8} aria-hidden="true" /><span>{t(label)}</span>
  </Link>;
}

function routeFrom(pathname: string): [string, string | null] {
  const path = pathname.replace(/\/+$/, '') || '/';
  if (path === '/console' || path === '/' || path === '/console/dashboard' || path === '/admin' || path === '/admin/dashboard') return ['dashboard', 'dashboard'];
  if (path === '/admin/login') return ['login', 'login'];
  if (path === '/console/tokens') return ['tokens', 'tokens'];
  if (path.startsWith('/console/tokens/')) return ['tokens', 'token-detail'];
  if (path === '/console/requests') return ['requests', 'requests'];
  if (path === '/console/setup') return ['setup', 'setup'];
  if (path === '/console/billing') return ['billing', 'billing'];
  if (path === '/admin/pools') return ['pools', 'pools'];
  if (path.startsWith('/admin/pools/')) return ['pools', 'pool-detail'];
  if (path === '/admin/users') return ['users', 'users'];
  if (path.startsWith('/admin/users/')) return ['users', 'user-detail'];
  if (path === '/admin/logs') return ['logs', 'logs'];
  if (path === '/admin/announcements') return ['announcements', 'announcements'];
  if (path === '/admin/settings') return ['settings', 'settings'];
  if (path === '/admin/billing') return ['billingCodes', 'billing'];
  return ['', null];
}

function RoutePage({ admin, base, resource }: { admin: boolean; base: string; resource: string | null }) {
  const { location } = useNavigation();
  const id = useMemo(() => {
    if (!location.pathname.startsWith('/admin/')) return undefined;
    const segment = base === 'pools' && resource === 'pool-detail' || base === 'users' && resource === 'user-detail' ? location.pathname.split('/').at(-1) : undefined;
    try { return segment ? decodeURIComponent(segment) : undefined; } catch { return undefined; }
  }, [location.pathname, base, resource]);
  if (resource === 'dashboard') return <Dashboard admin={admin} />;
  if (resource === 'tokens' || resource === 'token-detail') return <TokenPage detailId={resource === 'token-detail' ? idFrom(location.pathname) : undefined} />;
  if (resource === 'requests' || resource === 'logs') return <RequestPage admin={admin} />;
  if (resource === 'setup') return <SetupPage />;
  if (resource === 'billing' && !admin) return <BillingPage />;
  if (resource === 'billing' && admin) return <AdminBilling />;
  if ((resource === 'pools' || resource === 'pool-detail') && admin) return <PoolPage poolId={resource === 'pool-detail' ? id : undefined} />;
  if ((resource === 'users' || resource === 'user-detail') && admin) return <UserManagement userId={resource === 'user-detail' ? id : undefined} />;
  if (resource === 'announcements' && admin) return <AnnouncementsPage />;
  if (resource === 'settings' && admin) return <SettingsPage />;
  return null;
}

function idFrom(pathname: string) {
  const id = pathname.split('/').at(-1);
  try { return id ? decodeURIComponent(id) : undefined; } catch { return undefined; }
}
