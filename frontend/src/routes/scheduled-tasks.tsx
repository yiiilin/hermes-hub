import type {
  ApiClient,
  HermesScheduledTaskSnapshot,
  HermesSchedulerSnapshot,
} from "../api/client";
import { useI18n } from "../i18n";
import { useEffect, useMemo, useState } from "react";

type ScheduledTasksRouteProps = {
  active: boolean;
  apiClient: ApiClient;
};

type SchedulerTaskRow = {
  snapshot: HermesSchedulerSnapshot;
  task: HermesScheduledTaskSnapshot;
};

function formatSchedulerSnapshotTime(
  value: number | string | null | undefined,
  language: string,
): string {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  // Adapter 可能上送秒级 Unix 时间，也可能上送 ISO 字符串；展示层统一容错格式化。
  const timestamp =
    typeof value === "number" && value < 10_000_000_000 ? value * 1000 : value;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  return new Intl.DateTimeFormat(language === "zh" ? "zh-CN" : "en-US", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

export function ScheduledTasksRoute({ active, apiClient }: ScheduledTasksRouteProps) {
  const { language, t } = useI18n();
  const [snapshot, setSnapshot] = useState<HermesSchedulerSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const taskRows = useMemo<SchedulerTaskRow[]>(
    () => {
      if (!snapshot) {
        return [];
      }
      return (snapshot.tasks ?? []).map((task) => ({ snapshot, task }));
    },
    [snapshot],
  );

  async function refresh() {
    setLoading(true);
    setError(null);
    try {
      setSnapshot(await apiClient.workspaceHermesSchedulerSnapshot());
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : t("chat.requestFailed"));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (!active) {
      return;
    }
    let alive = true;
    setLoading(true);
    setError(null);
    void apiClient
      .workspaceHermesSchedulerSnapshot()
      .then((nextSnapshot) => {
        if (alive) {
          setSnapshot(nextSnapshot);
        }
      })
      .catch((nextError) => {
        if (alive) {
          setError(nextError instanceof Error ? nextError.message : t("chat.requestFailed"));
        }
      })
      .finally(() => {
        if (alive) {
          setLoading(false);
        }
      });

    return () => {
      alive = false;
    };
  }, [active, apiClient, t]);

  if (!active) {
    return null;
  }

  return (
    <section className="admin-page user-scheduler-page" id="user-scheduled-tasks">
      <div className="panel-heading">
        <h1>{t("admin.scheduledTasks")}</h1>
      </div>
      <div className="tab-actions">
        <button type="button" className="secondary" disabled={loading} onClick={() => void refresh()}>
          {loading ? t("common.loading") : t("admin.refresh")}
        </button>
      </div>
      {error ? <p className="error">{error}</p> : null}
      <div className="panel scheduler-panel">
        {taskRows.length === 0 ? (
          <div className="empty-state">
            <strong>{loading ? t("common.loading") : t("admin.noScheduledTasks")}</strong>
          </div>
        ) : (
          <div className="table-scroll">
            <table aria-label={t("admin.scheduledTasks")}>
              <thead>
                <tr>
                  <th>{t("admin.schedulerTask")}</th>
                  <th>{t("admin.schedule")}</th>
                  <th>{t("admin.nextRunAt")}</th>
                  <th>{t("admin.lastRunAt")}</th>
                  <th>{t("admin.status")}</th>
                  <th>{t("admin.source")}</th>
                  <th>{t("admin.instanceStatus")}</th>
                  <th>{t("admin.reportedAt")}</th>
                </tr>
              </thead>
              <tbody>
                {taskRows.map(({ snapshot: rowSnapshot, task }) => (
                  <tr key={`${rowSnapshot.hermes_instance_id}:${task.id}`}>
                    <td>
                      <span className="status-cell">
                        <span>{task.name || task.id}</span>
                        <span className="status-detail">
                          {task.enabled ? t("admin.enabled") : t("admin.disabled")} / {task.id}
                        </span>
                      </span>
                    </td>
                    <td>
                      <span className="status-cell">
                        <span>{task.schedule || "-"}</span>
                        <span className="status-detail">{task.timezone || "-"}</span>
                      </span>
                    </td>
                    <td>{formatSchedulerSnapshotTime(task.next_run_at, language)}</td>
                    <td>{formatSchedulerSnapshotTime(task.last_run_at, language)}</td>
                    <td>{task.status || "-"}</td>
                    <td>{task.source || "-"}</td>
                    <td>
                      <span className="status-cell">
                        <span>{rowSnapshot.instance_status || "-"}</span>
                        <span className="status-detail">
                          {rowSnapshot.scheduler_enabled
                            ? t("admin.schedulerEnabled")
                            : t("admin.schedulerDisabled")}
                          {" / "}
                          {t("admin.runningJobs")}: {rowSnapshot.running_jobs_count}
                        </span>
                      </span>
                    </td>
                    <td>{formatSchedulerSnapshotTime(rowSnapshot.reported_at, language)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </section>
  );
}
