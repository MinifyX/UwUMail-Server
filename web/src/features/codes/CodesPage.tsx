import { Copy, KeyRound, ShieldCheck } from "lucide-react";
import { Card, PageHeader } from "@/components/ui/Card";
import { Segmented, SettingRow, Toggle } from "@/components/ui/Field";
import { Hint } from "@/components/ui/Hint";
import { useT } from "@/i18n";
import { CODE_CLEANUPS, useCodeSettings } from "./codeSettings";

/**
 * How the chip above the mail list looks, with a made-up code. Only a picture: it copies nothing.
 * Origin uses a reserved name (RFC 2606), so it never points anywhere real.
 */
function CodeChipPreview() {
  const { t } = useT();
  return (
    <div className="flex flex-col gap-2 border-t border-hairline pt-4">
      <p className="text-[12px] font-bold tracking-wide text-muted uppercase">{t("codes.preview.title")}</p>
      <div
        aria-hidden
        className="inline-flex max-w-full items-center gap-2.5 self-start rounded-full border border-line bg-surface py-1 pr-1 pl-3"
      >
        <ShieldCheck className="size-4 shrink-0 text-pink-ink" />
        <span className="font-mono text-[15px] font-bold tabular-nums">482&#8194;913</span>
        <span className="flex min-w-0 items-center gap-1.5 text-[12px] text-muted">
          <span className="truncate">login.bank.example</span>
          <span className="tabular-nums">{t("codes.preview.left", { time: "4:32" })}</span>
        </span>
        <span className="flex size-7 shrink-0 items-center justify-center rounded-full text-muted">
          <Copy className="size-4" />
        </span>
      </div>
      <p className="text-[12px] text-muted">{t("codes.preview.caption")}</p>
    </div>
  );
}

export function CodesPage() {
  const { t } = useT();
  const settings = useCodeSettings();

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("codes.title")} intro={t("codes.intro")} />
      <div className="flex max-w-[760px] flex-col gap-5">
        <Card icon={KeyRound} title={t("codes.card.title")} intro={t("codes.card.intro")}>
          <div className="flex flex-col gap-4">
            <Toggle
              checked={settings.detect}
              onChange={(detect) => settings.set({ detect })}
              label={t("codes.detect.label")}
              description={t("codes.detect.description")}
            />
            <div className="border-t border-hairline" />
            <SettingRow label={t("codes.cleanup.label")} description={t(`codes.cleanup.description.${settings.cleanup}`)}>
              {({ describedBy }) => (
                <Segmented
                  label={t("codes.cleanup.label")}
                  describedBy={describedBy}
                  value={settings.cleanup}
                  onChange={(cleanup) => settings.set({ cleanup })}
                  options={CODE_CLEANUPS.map((value) => ({ value, label: t(`codes.cleanup.options.${value}`) }))}
                />
              )}
            </SettingRow>
            <Hint icon={ShieldCheck}>
              {t("codes.signed", { header: "Verification-Info" })}
            </Hint>
            <CodeChipPreview />
          </div>
        </Card>
        {/* TODO(main session): drop this note once the settings are stored on the server (codeSettings.ts). */}
        <p className="text-[12px] text-muted">{t("codes.localOnly")}</p>
      </div>
    </div>
  );
}
