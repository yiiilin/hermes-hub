import type { ApiClient } from "../api/client";
import { useI18n, type Language } from "../i18n";
import { FormEvent, useEffect, useState } from "react";

type PersonalSettingsRouteProps = {
  active: boolean;
  apiClient: ApiClient;
};

type PersonalSettingsTab = "personalization" | "password";

export function PersonalSettingsRoute({ active, apiClient }: PersonalSettingsRouteProps) {
  const { language, setLanguage, t } = useI18n();
  const [activeTab, setActiveTab] = useState<PersonalSettingsTab>("personalization");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [savingPassword, setSavingPassword] = useState(false);
  const [passwordSaved, setPasswordSaved] = useState(false);
  const [passwordError, setPasswordError] = useState<string | null>(null);

  useEffect(() => {
    if (!active || activeTab !== "password") {
      resetPasswordForm();
    }
  }, [active, activeTab]);

  if (!active) {
    return null;
  }

  function resetPasswordForm() {
    setNewPassword("");
    setConfirmPassword("");
    setPasswordSaved(false);
    setPasswordError(null);
  }

  async function savePassword(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPasswordSaved(false);
    setPasswordError(null);

    if (newPassword !== confirmPassword) {
      setPasswordError(t("personal.passwordMismatch"));
      return;
    }
    if (!newPassword.trim()) {
      setPasswordError(t("personal.passwordRequired"));
      return;
    }

    setSavingPassword(true);
    try {
      await apiClient.updatePassword(newPassword);
      resetPasswordForm();
      setPasswordSaved(true);
    } catch (cause) {
      setPasswordError(cause instanceof Error ? cause.message : t("personal.passwordSaveFailed"));
    } finally {
      setSavingPassword(false);
    }
  }

  function switchLanguage(nextLanguage: Language) {
    setLanguage(nextLanguage);
  }

  return (
    <section className="admin-page personal-settings-page" id="personal-settings">
      <div className="panel-heading">
        <h1>{t("personal.settings")}</h1>
      </div>
      <div className="settings-tabs" role="tablist" aria-label={t("personal.settings")}>
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === "personalization"}
          className={activeTab === "personalization" ? "active" : ""}
          onClick={() => setActiveTab("personalization")}
        >
          {t("personal.personalization")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === "password"}
          className={activeTab === "password" ? "active" : ""}
          onClick={() => setActiveTab("password")}
        >
          {t("personal.changePassword")}
        </button>
      </div>

      {activeTab === "personalization" ? (
        <section className="panel form personal-settings-panel">
          <fieldset className="form-section compact-form-section">
            <legend>{t("i18n.language")}</legend>
            <div className="language-switch personal-language-switch">
              <button
                type="button"
                className={language === "zh" ? "active" : ""}
                onClick={() => switchLanguage("zh")}
              >
                {t("i18n.chinese")}
              </button>
              <button
                type="button"
                className={language === "en" ? "active" : ""}
                onClick={() => switchLanguage("en")}
              >
                {t("i18n.english")}
              </button>
            </div>
          </fieldset>
        </section>
      ) : (
        <form className="panel form personal-settings-panel" onSubmit={(event) => void savePassword(event)}>
          {passwordError ? <p className="error">{passwordError}</p> : null}
          {passwordSaved ? <p className="copy-line">{t("personal.passwordSaved")}</p> : null}
          <label>
            {t("personal.newPassword")}
            <input
              type="password"
              autoComplete="new-password"
              value={newPassword}
              onChange={(event) => {
                setNewPassword(event.target.value);
                setPasswordSaved(false);
              }}
            />
          </label>
          <label>
            {t("personal.confirmPassword")}
            <input
              type="password"
              autoComplete="new-password"
              value={confirmPassword}
              onChange={(event) => {
                setConfirmPassword(event.target.value);
                setPasswordSaved(false);
              }}
            />
          </label>
          <div className="button-row">
            <button type="submit" disabled={savingPassword}>
              {savingPassword ? t("common.loading") : t("personal.savePassword")}
            </button>
          </div>
        </form>
      )}
    </section>
  );
}
