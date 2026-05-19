export function LoginRoute() {
  return (
    <section className="panel login-panel" aria-labelledby="login-title">
      <h1 id="login-title">Hermes Hub</h1>
      <form className="form">
        <label>
          Email
          <input name="email" type="email" autoComplete="email" />
        </label>
        <label>
          Password
          <input name="password" type="password" autoComplete="current-password" />
        </label>
        <button type="button">Sign in</button>
      </form>
    </section>
  );
}
