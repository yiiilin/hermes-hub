import type { ApiClient, User } from "../api/client";
import { useI18n } from "../i18n";
import { Bot } from "lucide-react";
import { FormEvent, useEffect, useMemo, useState } from "react";

type LoginRouteProps = {
  apiClient: ApiClient;
  onAuthenticated: (user: User) => void;
};

type AuthMode = "login" | "bootstrap" | "invite";

export function LoginRoute({ apiClient, onAuthenticated }: LoginRouteProps) {
  const { t } = useI18n();
  const inviteFromUrl = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get("invite") ?? params.get("invite_token") ?? "";
  }, []);
  const [mode, setMode] = useState<AuthMode>(inviteFromUrl ? "invite" : "login");
  const [checkingBootstrap, setCheckingBootstrap] = useState(!inviteFromUrl);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const isRegistering = mode !== "login";

  useEffect(() => {
    let alive = true;

    if (inviteFromUrl) {
      setCheckingBootstrap(false);
      return () => {
        alive = false;
      };
    }

    void apiClient
      .bootstrapStatus()
      .then((status) => {
        if (alive && status.bootstrap_open) {
          setMode("bootstrap");
        }
      })
      .catch(() => {
        if (alive) {
          setMode("login");
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
  }, [apiClient, inviteFromUrl]);

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
            ? await apiClient.registerWithInvite(inviteFromUrl, email, password)
            : await apiClient.login(email, password);
      onAuthenticated(user);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("auth.authFailed"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="auth-shell">
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
        {!inviteFromUrl && !checkingBootstrap ? (
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
    </main>
  );
}
