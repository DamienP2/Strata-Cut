/* Système › Supervision › Alertes: what goes wrong told to a webhook and by e-mail. */
import { useState } from "react";
import { Bell, Mail, Send, Webhook } from "lucide-react";
import { t } from "../i18n";
import { api } from "../store";
import { PageHead } from "../pages/SidePage";
import { Dropdown } from "../ui/Dropdown";
import { NumberField } from "../ui/fields";
import { Card, Field, SaveBar, Switch } from "./ui";
import { useServerConfig } from "./serverConfig";
import { friendlyError } from "../ui/serverError";

interface AlertsCfg {
  enabled: boolean;
  language: string;
  webhook: string;
  emailTo: string;
  smtpHost: string;
  smtpPort: number;
  smtpSecurity: string;
  smtpUser: string;
  smtpPassword: string;
  smtpFrom: string;
  delaySecs: number;
  signal: boolean;
  content: boolean;
  streams: boolean;
  disk: boolean;
  restart: boolean;
}

const DEFAULTS: AlertsCfg = {
  enabled: false,
  language: "fr",
  webhook: "",
  emailTo: "",
  smtpHost: "",
  smtpPort: 587,
  smtpSecurity: "starttls",
  smtpUser: "",
  smtpPassword: "",
  smtpFrom: "",
  delaySecs: 30,
  signal: true,
  content: true,
  streams: true,
  disk: true,
  restart: true,
};

type Result = { ok: boolean; error?: string };

export function Alerts() {
  const { cfg, setCfg, dirty, saving, save, discard } = useServerConfig();
  const [test, setTest] = useState<{
    busy: boolean;
    webhook?: Result;
    email?: Result;
    error?: string;
  }>({ busy: false });
  if (!cfg) return <div className="loading">{t("Loading…")}</div>;
  const a: AlertsCfg = {
    ...DEFAULTS,
    ...((cfg.alerts as Partial<AlertsCfg> | undefined) ?? {}),
  };
  const set = (p: Partial<AlertsCfg>) =>
    setCfg((c) => ({
      ...c,
      alerts: {
        ...DEFAULTS,
        ...((c.alerts as Partial<AlertsCfg> | undefined) ?? {}),
        ...p,
      },
    }));

  const runTest = async () => {
    setTest({ busy: true });
    try {
      setTest({
        busy: false,
        ...(await api<{ webhook?: Result; email?: Result }>(
          "POST",
          "/api/system/alerts/test",
          a,
        )),
      });
    } catch (e) {
      setTest({ busy: false, error: friendlyError((e as Error).message) });
    }
  };
  const said = (r?: Result) =>
    r ? (
      r.ok ? (
        <span className="ok-text">{t("Sent")}</span>
      ) : (
        <span className="bad-text">{t(r.error ?? "")}</span>
      )
    ) : null;

  const events: [keyof AlertsCfg, string][] = [
    ["signal", "A source that should be received is not"],
    ["content", "Black picture, frozen picture, silence"],
    ["streams", "A live lost on its way, or that could not start"],
    ["disk", "A disk almost full, a loop cut short for room"],
    ["restart", "The server started again after an unexpected stop"],
  ];

  return (
    <>
      <PageHead title={t("Alerts")}>
        <button
          type="button"
          className="btn"
          disabled={test.busy || (!a.webhook && !a.emailTo)}
          onClick={() => void runTest()}
        >
          <Send size={14} /> {t("Send a test")}
        </button>
      </PageHead>
      {(test.webhook || test.email || test.error) && (
        <div className="alert-test">
          {test.error && <span className="bad-text">{test.error}</span>}
          {test.webhook && (
            <span>
              {t("Webhook")} : {said(test.webhook)}
            </span>
          )}
          {test.email && (
            <span>
              {t("E-mail")} : {said(test.email)}
            </span>
          )}
        </div>
      )}
      <Card
        icon={<Bell size={16} />}
        title={t("Alerts")}
        actions={
          <Switch
            checked={a.enabled}
            onChange={(enabled) => set({ enabled })}
            label={a.enabled ? t("On") : t("Off")}
          />
        }
      >
        <div className="set-grid two">
          <Field label={t("Told after")} group>
            <div className="set-inline">
              <NumberField
                value={a.delaySecs}
                min={5}
                max={3600}
                onChange={(delaySecs) => set({ delaySecs })}
              />
              <span className="muted">s</span>
            </div>
          </Field>
          <Field label={t("Language of the messages")} group>
            <Dropdown
              value={a.language}
              options={[
                { value: "fr", label: "Français" },
                { value: "en", label: "English" },
              ]}
              onChange={(language) => set({ language })}
            />
          </Field>
        </div>
        <div className="alert-events">
          {events.map(([k, label]) => (
            <Switch
              key={k}
              checked={a[k] as boolean}
              onChange={(v) => set({ [k]: v } as Partial<AlertsCfg>)}
              label={t(label)}
            />
          ))}
        </div>
      </Card>
      <Card icon={<Webhook size={16} />} title={t("Webhook")}>
        <Field
          label={t("Address")}
          hint={t(
            "Slack, Teams, Discord, Mattermost, or any service that takes JSON.",
          )}
        >
          <input
            value={a.webhook}
            placeholder="https://hooks.slack.com/services/…"
            spellCheck={false}
            onChange={(e) => set({ webhook: e.target.value })}
          />
        </Field>
      </Card>
      <Card icon={<Mail size={16} />} title={t("E-mail")}>
        <div className="set-grid two">
          <Field label={t("Recipients")} hint={t("Separated by commas.")}>
            <input
              value={a.emailTo}
              placeholder="regie@example.com"
              spellCheck={false}
              onChange={(e) => set({ emailTo: e.target.value })}
            />
          </Field>
          <Field label={t("Sender")}>
            <input
              value={a.smtpFrom}
              placeholder="strata-cut@example.com"
              spellCheck={false}
              onChange={(e) => set({ smtpFrom: e.target.value })}
            />
          </Field>
          <Field label={t("Mail server")}>
            <input
              value={a.smtpHost}
              placeholder="smtp.example.com"
              spellCheck={false}
              onChange={(e) => set({ smtpHost: e.target.value })}
            />
          </Field>
          <div className="set-grid two">
            <Field label={t("Port")} group>
              <NumberField
                value={a.smtpPort}
                min={1}
                max={65535}
                onChange={(smtpPort) => set({ smtpPort })}
              />
            </Field>
            <Field label={t("Security")} group>
              <Dropdown
                value={a.smtpSecurity}
                options={[
                  { value: "starttls", label: "STARTTLS", detail: "587" },
                  { value: "tls", label: "TLS", detail: "465" },
                  { value: "none", label: t("None") },
                ]}
                onChange={(smtpSecurity) =>
                  set({
                    smtpSecurity,
                    smtpPort:
                      smtpSecurity === "tls"
                        ? 465
                        : smtpSecurity === "starttls"
                          ? 587
                          : 25,
                  })
                }
              />
            </Field>
          </div>
          <Field label={t("Mail user")}>
            <input
              value={a.smtpUser}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => set({ smtpUser: e.target.value })}
            />
          </Field>
          <Field label={t("Password")}>
            <input
              type="password"
              value={a.smtpPassword}
              autoComplete="new-password"
              onChange={(e) => set({ smtpPassword: e.target.value })}
            />
          </Field>
        </div>
      </Card>
      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={() => void save()}
        onDiscard={discard}
      />
    </>
  );
}
