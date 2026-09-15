import clsx from "clsx";
import { CircleAlert, CircleCheck, CircleDashed, CircleHelp, CircleX, TriangleAlert } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { CopyButton } from "@/components/ui/Card";
import { useT } from "@/i18n";
import type { CheckStatus, RecordCheck } from "@/lib/api";

const STATUS: Record<CheckStatus, { icon: LucideIcon; className: string }> = {
  ok: { icon: CircleCheck, className: "bg-success-tint text-success" },
  warning: { icon: TriangleAlert, className: "bg-warning-tint text-warning" },
  missing: { icon: CircleAlert, className: "bg-danger-tint text-danger" },
  wrong: { icon: CircleX, className: "bg-danger-tint text-danger" },
  error: { icon: CircleHelp, className: "bg-warning-tint text-warning" },
};

/** A recommended record that is not there yet is a suggestion, not an alarm. */
const RECOMMENDED = { icon: CircleDashed, className: "bg-elevated text-muted" };

/** "DNS is fine" and friends, for lists and headers. */
export function DnsStatusPill({ status }: { status: CheckStatus | null }) {
  const { t } = useT();
  if (!status) {
    return (
      <span className="inline-flex h-6 items-center rounded-full border border-line px-2.5 text-[12px] font-semibold text-muted">
        {t("domains.notChecked")}
      </span>
    );
  }
  const { icon: Icon, className } = STATUS[status];
  return (
    <span
      className={clsx("inline-flex h-6 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold", className)}
    >
      <Icon className="size-3.5" aria-hidden />
      {t(`domains.dnsStatus.${status}`)}
    </span>
  );
}

const KNOWN_NOTES = [
  "mxUpstream",
  "mxElsewhere",
  "spfTooLoose",
  "spfMultiple",
  "spfNotAllowed",
  "spfUnverified",
  "spfInvalid",
  "dmarcNone",
  "dmarcInvalid",
  "dmarcMultiple",
  "dmarcReportsElsewhere",
  "dkimMismatch",
  "lookupFailed",
  "srvElsewhere",
  "tlsRptElsewhere",
  "tlsRptMultiple",
  "mtaStsOldId",
  "mtaStsMultiple",
  "mtaStsFetchFailed",
  "mtaStsPolicyDiffers",
  "mtaStsPolicyInvalid",
];

function Value({ value, label }: { value: string; label: string }) {
  return (
    <div className="flex items-start gap-1 rounded-control bg-canvas px-2.5 py-1.5">
      <code className="min-w-0 flex-1 text-[12px] break-all whitespace-pre-wrap select-all">{value.trimEnd()}</code>
      <CopyButton value={value} label={label} />
    </div>
  );
}

/** One DNS record: what to publish, what was found, and why it matters. */
export function RecordRow({ record, domain, explain }: { record: RecordCheck; domain: string; explain: boolean }) {
  const { t } = useT();
  const recommended = record.optional && record.status === "missing";
  const { icon: Icon, className } = recommended ? RECOMMENDED : STATUS[record.status];
  const note = record.note && KNOWN_NOTES.includes(record.note) ? t(`domains.detail.notes.${record.note}`) : null;
  // What to publish matters when something is off, or when a better value is suggested (e.g. DMARC).
  const expectedShown = record.status !== "ok" || !record.found.includes(record.expected);
  const isDns = record.recordType !== "HTTPS";
  return (
    <li className="flex flex-col gap-2 border-b border-hairline py-4 first:pt-0 last:border-b-0 last:pb-0">
      <div className="flex flex-wrap items-center gap-2">
        <span
          className={clsx(
            "inline-flex h-6 items-center gap-1 rounded-full px-2.5 text-[12px] font-semibold",
            className,
          )}
        >
          <Icon className="size-3.5" aria-hidden />
          {recommended ? t("domains.detail.status.recommended") : t(`domains.detail.status.${record.status}`)}
        </span>
        <span className="text-sm font-bold">{t(`domains.detail.kinds.${record.kind}`)}</span>
        {isDns && record.recordType.toLowerCase() !== record.kind && (
          <span className="text-[12px] text-muted">{record.recordType}</span>
        )}
        {record.keyState && (
          <span className="text-[12px] font-semibold text-muted">
            · {t(`domains.detail.keyState.${record.keyState}`)}
          </span>
        )}
      </div>
      {explain && <p className="text-[13px] text-muted">{t(`domains.detail.purpose.${record.kind}`)}</p>}
      {note && (
        <p className={clsx("text-[13px]", record.status === "ok" ? "text-muted" : "font-medium text-ink")}>{note}</p>
      )}
      <div className="grid gap-2 md:grid-cols-[120px_1fr] md:items-start">
        <span className="pt-1.5 text-[12px] font-semibold text-muted">
          {isDns ? t("domains.detail.name") : t("domains.detail.address")}
        </span>
        <Value value={record.name} label={t("domains.detail.copyName")} />
        {expectedShown && (
          <>
            <span className="pt-1.5 text-[12px] font-semibold text-muted">
              {isDns ? t("domains.detail.value") : t("domains.detail.content")}
            </span>
            <Value value={record.expected} label={t("domains.detail.copyValue")} />
          </>
        )}
        <span className="pt-1.5 text-[12px] font-semibold text-muted">{t("domains.detail.found")}</span>
        <div className="flex flex-col gap-1 pt-1.5">
          {record.found.length === 0 ? (
            <span className="text-[13px] text-faint">{t("domains.detail.foundNothing")}</span>
          ) : (
            record.found.map((value) => (
              <code key={value} className="text-[12px] break-all whitespace-pre-wrap text-muted">
                {value.trimEnd()}
              </code>
            ))
          )}
        </div>
      </div>
      {explain && isDns && record.name !== domain && record.name.endsWith(`.${domain}`) && (
        <p className="text-[12px] text-faint">{t("domains.detail.nameHint", { domain })}</p>
      )}
    </li>
  );
}

/** The records that matter first, then the recommended ones under their own heading. */
export function RecordList({ records, domain, explain }: { records: RecordCheck[]; domain: string; explain: boolean }) {
  const { t } = useT();
  const required = records.filter((record) => !record.optional);
  const optional = records.filter((record) => record.optional);
  const row = (record: RecordCheck) => (
    <RecordRow key={`${record.kind}-${record.name}`} record={record} domain={domain} explain={explain} />
  );
  return (
    <div className="flex flex-col gap-4">
      <ul className="flex flex-col">{required.map(row)}</ul>
      {optional.length > 0 && (
        <div className="flex flex-col gap-3 border-t border-hairline pt-4">
          <div>
            <h3 className="text-sm font-bold">{t("domains.detail.recommendedTitle")}</h3>
            {explain && <p className="mt-0.5 text-[13px] text-muted">{t("domains.detail.recommendedIntro")}</p>}
          </div>
          <ul className="flex flex-col">{optional.map(row)}</ul>
        </div>
      )}
    </div>
  );
}
