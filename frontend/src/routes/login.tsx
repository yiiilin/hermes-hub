import type { ApiClient, User } from "../api/client";
import { FormEvent, useMemo, useState } from "react";

type LoginRouteProps = {
  apiClient: ApiClient;
  onAuthenticated: (user: User) => void;
};

type AuthMode = "login" | "bootstrap" | "invite";

export function LoginRoute({ apiClient, onAuthenticated }: LoginRouteProps) {
  const inviteFromUrl = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get("invite") ?? params.get("invite_token") ?? "";
  }, []);
  const [mode, setMode] = useState<AuthMode>(inviteFromUrl ? "invite" : "login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [inviteToken, setInviteToken] = useState(inviteFromUrl);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(null);

    try {
      const user =
        mode === "bootstrap"
          ? await apiClient.bootstrapRegister(email, password)
          : mode === "invite"
            ? await apiClient.registerWithInvite(inviteToken, email, password)
            : await apiClient.login(email, password);
      onAuthenticated(user);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Authentication failed");
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="panel login-panel" aria-labelledby="login-title">
      <h1 id="login-title">Hermes Hub</h1>
      <div className="segmented" role="tablist" aria-label="Authentication mode">
        <button type="button" className={mode === "login" ? "active" : ""} onClick={() => setMode("login")}>
          Sign in
        </button>
        <button type="button" className={mode === "invite" ? "active" : ""} onClick={() => setMode("invite")}>
          Invite
        </button>
        <button type="button" className={mode === "bootstrap" ? "active" : ""} onClick={() => setMode("bootstrap")}>
          First admin
        </button>
      </div>
      <form className="form" onSubmit={submit}>
        {mode === "invite" ? (
          <label>
            Invite token
            <input
              name="invite"
              value={inviteToken}
              onChange={(event) => setInviteToken(event.target.value)}
              required
            />
          </label>
        ) : null}
        <label>
          Email
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
          Password
          <input
            name="password"
            type="password"
            autoComplete={mode === "login" ? "current-password" : "new-password"}
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            required
          />
        </label>
        {error ? <p className="error">{error}</p> : null}
        <button type="submit" disabled={busy}>
          {busy ? "Working" : mode === "bootstrap" ? "Create admin" : mode === "invite" ? "Register" : "Sign in"}
        </button>
      </form>
    </section>
  );
}
