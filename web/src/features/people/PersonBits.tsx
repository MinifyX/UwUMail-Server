import { AvatarBlob, Tag, type TagTone, cx, ProgressBar } from "@uwusuite/design";
import { useT } from "@/i18n";
import type { Person, PersonStatus } from "@/lib/api";
import { formatBytes } from "@/lib/format";

export function PersonAvatar({
  person,
  size = "md",
}: {
  person: Pick<Person, "name" | "login" | "status" | "picture">;
  size?: "sm" | "md" | "lg";
}) {
  return (
    <span
      aria-hidden
      className={cx(
        "flex shrink-0 items-center justify-center overflow-hidden rounded-full",
        person.status === "deleted" || person.status === "disabled" ? "opacity-45 grayscale" : undefined,
        size === "sm" && "size-8",
        size === "md" && "size-10",
        size === "lg" && "size-14",
      )}
    >
      {person.picture ? (
        <img src={person.picture} alt="" className="size-full object-cover" />
      ) : (
        <AvatarBlob seed={person.login} />
      )}
    </span>
  );
}

const STATUS_TONES: Record<PersonStatus, TagTone> = {
  active: "success",
  invited: "pink",
  disabled: "warning",
  deleted: "danger",
};

export function StatusPill({ status }: { status: PersonStatus }) {
  const { t } = useT();
  return <Tag tone={STATUS_TONES[status]}>{t(`people.status.${status}`)}</Tag>;
}

/** Says that this mailbox belongs to a program, not a person. */
export function ServicePill() {
  const { t } = useT();
  return <Tag tone="neutral">{t("people.service")}</Tag>;
}

export function AdminPill() {
  const { t } = useT();
  return <Tag tone="pink">{t("people.admin")}</Tag>;
}

/** A thin bar for storage with the amount beside it; without a limit only the used amount is shown. */
export function StorageLine({ person }: { person: Pick<Person, "usedBytes" | "quotaBytes"> }) {
  const { t, i18n } = useT();
  const used = formatBytes(person.usedBytes, i18n.language);
  if (person.quotaBytes <= 0)
    return <span className="text-[13px] text-muted tabular-nums">{t("people.storageUsed", { used })}</span>;
  return (
    <span className="flex min-w-0 items-center gap-2">
      <ProgressBar kind="usage" share={person.usedBytes / person.quotaBytes} className="min-w-12 flex-1" />
      <span className="shrink-0 text-[13px] text-muted tabular-nums">
        {t("people.storageOf", { used, quota: formatBytes(person.quotaBytes, i18n.language) })}
      </span>
    </span>
  );
}
