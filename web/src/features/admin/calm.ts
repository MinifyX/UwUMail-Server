import type { AdminAlert, AlertsView, Health, HealthFinding, HealthLevel } from "@/lib/api";

/** Something the calm view asks the admin to look at. */
export type CalmItem = { key: string; level: "warning" | "problem"; link: string | null } & (
  { source: "health"; finding: HealthFinding } | { source: "alert"; alert: AdminAlert }
);

const ORDER: HealthLevel[] = ["ok", "unknown", "warning", "problem"];

/** Alerts the health overview does not show itself: backups and certificate renewal. */
function beyondHealth(alert: AdminAlert): boolean {
  return alert.kind === "backup" || alert.code === "certRenewalFailing";
}

/** Everything yellow or red, the red first. */
export function calmItems(health: Health | undefined, alerts: AlertsView | undefined): CalmItem[] {
  const items: CalmItem[] = [];
  for (const area of health?.areas ?? []) {
    area.findings.forEach((finding, index) => {
      if (finding.level !== "warning" && finding.level !== "problem") return;
      items.push({
        key: `${area.area}-${finding.code}-${index}`,
        level: finding.level,
        link: finding.link ?? null,
        source: "health",
        finding,
      });
    });
  }
  for (const alert of alerts?.open ?? []) {
    if (!beyondHealth(alert) || alert.level === "info") continue;
    items.push({ key: `alert-${alert.id}`, level: alert.level, link: alert.link, source: "alert", alert });
  }
  return items.sort((a, b) => ORDER.indexOf(b.level) - ORDER.indexOf(a.level));
}

/** The colour of the one traffic light: the worst of the health overview and the other alerts. */
export function calmLevel(health: Health | undefined, alerts: AlertsView | undefined): HealthLevel {
  if (!health) return "unknown";
  return calmItems(health, alerts).reduce<HealthLevel>(
    (worst, item) => (ORDER.indexOf(item.level) > ORDER.indexOf(worst) ? item.level : worst),
    health.level,
  );
}
