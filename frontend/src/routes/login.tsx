import type { ApiClient, LdapPublicConfig, OidcPublicConfig, User } from "../api/client";
import { useI18n } from "../i18n";
import { Bot } from "lucide-react";
import { FormEvent, useEffect, useMemo, useState } from "react";

type LoginRouteProps = {
  apiClient: ApiClient;
  embedded?: boolean;
  onAuthenticated: (user: User) => void;
};

type AuthMode = "login" | "bootstrap" | "invite";

export function LoginRoute({ apiClient, embedded = false, onAuthenticated }: LoginRouteProps) {
  const { t } = useI18n();
  const inviteFromUrl = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get("invite") ?? params.get("invite_token") ?? "";
  }, []);
  const [inviteToken, setInviteToken] = useState(inviteFromUrl);
  const [mode, setMode] = useState<AuthMode>(inviteToken ? "invite" : "login");
  const [checkingBootstrap, setCheckingBootstrap] = useState(!inviteToken);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [oidc, setOidc] = useState<OidcPublicConfig | null>(null);
  const [ldap, setLdap] = useState<LdapPublicConfig | null>(null);
  const [bootstrapOpen, setBootstrapOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const isRegistering = mode !== "login";

  useEffect(() => {
    let alive = true;

    void Promise.all([
      apiClient.bootstrapStatus(),
      apiClient.oidcConfig(),
      apiClient.ldapConfig(),
    ])
      .then(([status, oidcConfig, ldapConfig]) => {
        if (alive) {
          setOidc(oidcConfig);
          setLdap(ldapConfig);
          setBootstrapOpen(status.bootstrap_open);
        }
        if (alive && !inviteToken && status.bootstrap_open) {
          setMode("bootstrap");
        }
      })
      .catch(() => {
        if (alive) {
          setBootstrapOpen(false);
          if (!inviteToken) {
            setMode("login");
          }
        }
      })
      .finally(() => {
        if (alive) {
          setCheckingBootstrap(false);
        }
      });

    return () => {
      alive = false;
    };
  }, [apiClient, inviteToken]);

  async function signInWithLdap() {
    setBusy(true);
    setError(null);

    try {
      onAuthenticated(await apiClient.ldapLogin(email, password));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("auth.authFailed"));
    } finally {
      setBusy(false);
    }
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(null);

    try {
      if (isRegistering && password !== confirmPassword) {
        throw new Error(t("auth.passwordMismatch"));
      }

      const user =
        mode === "bootstrap"
          ? await apiClient.bootstrapRegister(email, password)
          : mode === "invite"
            ? await apiClient.registerWithInvite(inviteToken, email, password)
            : await apiClient.login(email, password);
      if (mode === "invite") {
        // 邀请注册只创建账号，不建立登录 cookie；成功后回到登录页让用户正式登录。
        clearInviteTokenFromUrl();
        setInviteToken("");
        setMode("login");
        setPassword("");
        setConfirmPassword("");
        return;
      }
      onAuthenticated(user);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("auth.authFailed"));
    } finally {
      setBusy(false);
    }
  }

  const Shell = embedded ? "div" : "main";

  return (
    <Shell className="auth-shell">
      <section className="auth-card" aria-labelledby="login-title">
        <div className="auth-brand" aria-hidden="true">
          <Bot size={28} />
        </div>
        <h1 id="login-title">Hermes Hub</h1>
        <p className="auth-subtitle">
          {isRegistering ? t("auth.createSubtitle") : t("auth.signInSubtitle")}
        </p>
        <form className="form" onSubmit={submit}>
          <label>
            {t("auth.email")}
            <input
              name="email"
              type="email"
              autoComplete="email"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              required
            />
          </label>
          <label>
            {t("auth.password")}
            <input
              name="password"
              type="password"
              autoComplete={mode === "login" ? "current-password" : "new-password"}
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              required
            />
          </label>
          {isRegistering ? (
            <label>
              {t("auth.confirmPassword")}
              <input
                name="confirm-password"
                type="password"
                autoComplete="new-password"
                value={confirmPassword}
                onChange={(event) => setConfirmPassword(event.target.value)}
                required
              />
            </label>
          ) : null}
          {error ? <p className="error">{error}</p> : null}
          <button type="submit" disabled={busy || checkingBootstrap}>
            {checkingBootstrap
              ? t("auth.loading")
              : busy
                ? t("auth.working")
                : isRegistering
                  ? t("auth.createAccount")
                  : t("auth.signIn")}
          </button>
        </form>
        {!isRegistering && oidc?.enabled ? (
          <a className="oidc-button" href="/api/auth/oidc/start">
            {t("auth.signInWith", { provider: oidc.display_name })}
          </a>
        ) : null}
        {!isRegistering && ldap?.enabled ? (
          <button
            type="button"
            className="oidc-button"
            disabled={busy || checkingBootstrap}
            onClick={() => void signInWithLdap()}
          >
            {t("auth.signInWith", { provider: ldap.display_name })}
          </button>
        ) : null}
        {!inviteToken && !checkingBootstrap && (isRegistering || bootstrapOpen) ? (
          <button
            type="button"
            className="text-button"
            onClick={() => {
              setError(null);
              setPassword("");
              setConfirmPassword("");
              setMode(isRegistering ? "login" : "bootstrap");
            }}
          >
            {isRegistering ? t("auth.accountExists") : t("auth.bootstrapHint")}
          </button>
        ) : null}
      </section>
    </Shell>
  );
}

function clearInviteTokenFromUrl() {
  const url = new URL(window.location.href);
  url.searchParams.delete("invite");
  url.searchParams.delete("invite_token");
  const nextPath = `${url.pathname}${url.search}${url.hash}`;
  window.history.replaceState({}, "", nextPath);
}
