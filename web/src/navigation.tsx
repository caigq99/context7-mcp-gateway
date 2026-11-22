import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import type { AnchorHTMLAttributes, ReactNode } from 'react';

const NavigationContext = createContext<{ location: URL; navigate: (path: string) => void } | null>(null);

export function NavigationProvider({ children }: { children: ReactNode }) {
  const [location, setLocation] = useState(() => new URL(window.location.href));
  useEffect(() => {
    const onPop = () => setLocation(new URL(window.location.href));
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);
  const navigate = useCallback((path: string) => {
    const next = new URL(path, window.location.origin);
    if (next.origin !== window.location.origin) return;
    if (next.href === window.location.href) return;
    window.history.pushState(null, '', next);
    setLocation(next);
    window.scrollTo(0, 0);
  }, []);
  const value = useMemo(() => ({ location, navigate }), [location, navigate]);
  return <NavigationContext.Provider value={value}>{children}</NavigationContext.Provider>;
}

export function useNavigation() {
  const context = useContext(NavigationContext);
  if (!context) throw new Error('Missing navigation provider');
  return context;
}

export function Link({ href, onClick, ...props }: AnchorHTMLAttributes<HTMLAnchorElement> & { href: string }) {
  const { navigate } = useNavigation();
  return (
    <a
      {...props}
      href={href}
      onClick={(event) => {
        onClick?.(event);
        if (!event.defaultPrevented && event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey
          && (!props.target || props.target === '_self')) {
          event.preventDefault();
          navigate(href);
        }
      }}
    />
  );
}
