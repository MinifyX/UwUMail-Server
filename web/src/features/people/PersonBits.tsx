import clsx from "clsx";
import { useT } from "@/i18n";
import type { Person, PersonStatus } from "@/lib/api";
import { formatBytes } from "@/lib/format";

const AVATAR_COLORS = [
  "bg-[var(--uwu-account-pink)]",
  "bg-[var(--uwu-account-violet)]",
  "bg-[var(--uwu-account-sky)]",
  "bg-[var(--uwu-account-mint)]",
  "bg-[var(--uwu-account-amber)]",
  "bg-[var(--uwu-account-coral)]",
];

function colorFor(login: string) {
  let hash = 0;
  for (const char of login) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return AVATAR_COLORS[hash % AVATAR_COLORS.length];
}

export function initials(person: Pick<Person, "name" | "login">) {
  const words = (person.name || person.login.split("@")[0] || "?").split(/[\s._-]+/).filter(Boolean);
  return ((words[0]?.[0] ?? "?") + (words.length > 1 ? (words[words.length - 1]?.[0] ?? "") : "")).toUpperCase();
}

export function PersonAvatar({
  person,
  size = "md",
}: {
  person: Pick<Person, "name" | "login" | "status">;
  size?: "sm" | "md" | "lg";
}) {
  return (
    <span
      aria-hidden
      className={clsx(
        "flex shrink-0 items-center justify-center rounded-full font-bold text-white",
        colorFor(person.login),
        person.status === "deleted" || person.status === "disabled" ? "opacity-45 grayscale" : undefined,
        size === "sm" && "size-8 text-[12px]",
        size === "md" && "size-10 text-sm",
        size === "lg" && "size-14 text-lg",
      )}
    >
      {initials(person)}
    </span>
  );
}

const STATUS_STYLES: Record<PersonStatus, string> = {
  active: "bg-success-tint text-success",
  invited: "bg-pink-tint text-pink-ink",
  disabled: "bg-warning-tint text-warning",
  deleted: "bg-danger-tint text-danger",
};

export function StatusPill({ status }: { status: PersonStatus }) {
  const { t } = useT();
  return (
    <span
      className={clsx(
        "inline-flex h-6 items-center rounded-full px-2.5 text-[12px] font-semibold",
        STATUS_STYLES[status],
      )}
    >
      {t(`people.status.${status}`)}
    </span>
  );
}

export function AdminPill() {
  const { t } = useT();
  return (
    <span className="inline-flex h-6 items-center rounded-full border border-line px-2.5 text-[12px] font-semibold text-muted">
      {t("people.admin")}
    </span>
  );
}

/** A thin bar for storage; without a limit only the used amount is shown. */
export function StorageLine({ person }: { person: Pick<Person, "usedBytes" | "quotaBytes"> }) {
  const { t, i18n } = useT();
  const used = formatBytes(person.usedBytes, i18n.language);
  if (person.quotaBytes <= 0)
    return <span className="text-[13px] text-muted">{t("people.storageUsed", { used })}</span>;
  const share = Math.min(1, person.usedBytes / person.quotaBytes);
  return (
    <span className="flex min-w-0 flex-col gap-1">
      <span className="text-[13px] text-muted">
        {t("people.storageOf", { used, quota: formatBytes(person.quotaBytes, i18n.language) })}
      </span>
      <span className="h-1.5 w-full overflow-hidden rounded-full bg-canvas">
        <span
          className={clsx(
            "block h-full rounded-full",
            share > 0.9 ? "bg-danger" : share > 0.75 ? "bg-warning" : "bg-pink",
          )}
          style={{ width: `${share > 0 ? Math.max(share * 100, 3) : 0}%` }}
        />
      </span>
    </span>
  );
}
