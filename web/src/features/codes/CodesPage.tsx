import { Segmented, Toggle, Card, Hint, ICONS, Icon } from "@uwusuite/design";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { PageHeader } from "@/components/ui/Page";
import { SettingRow } from "@/components/ui/SettingRow";
import { useSession } from "@/features/session/session";
import { useT } from "@/i18n";
import { api } from "@/lib/api";
import { useErrorText } from "@/lib/errors";
import { toast } from "@/state/toasts";
import { CODE_CLEANUPS, type CodeSettings, codeSettingsFromPreferences, codeSettingsPatch } from "./codeSettings";

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
        <Icon icon={ICONS.securityCheck} className="size-4 shrink-0 text-pink-ink" />
        <span className="font-mono text-[15px] font-bold tabular-nums">482&#8194;913</span>
        <span className="flex min-w-0 items-center gap-1.5 text-[12px] text-muted">
          <span className="truncate">login.bank.example</span>
          <span className="tabular-nums">{t("codes.preview.left", { time: "4:32" })}</span>
        </span>
        <span className="flex size-7 shrink-0 items-center justify-center rounded-full text-muted">
          <Icon icon={ICONS.copy} className="size-4" />
        </span>
      </div>
      <p className="text-[12px] text-muted">{t("codes.preview.caption")}</p>
    </div>
  );
}

/**
 * Stored settings with the change being saved laid over them, so the controls answer at once.
 * A failed save drops the change again.
 */
function useCodeSettings() {
  const { t } = useT();
  const errorText = useErrorText();
  const queryClient = useQueryClient();
  const stored = codeSettingsFromPreferences(useSession().data?.preferences);
  const [pending, setPending] = useState<Partial<CodeSettings>>({});
  const save = useMutation({
    mutationFn: (changes: Partial<CodeSettings>) =>
      api<Record<string, unknown>>("/api/account/preferences", { method: "PATCH", body: codeSettingsPatch(changes) }),
    onMutate: (changes) => setPending((current) => ({ ...current, ...changes })),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["session"] });
      setPending({});
      toast(t("codes.saved"), "success");
    },
    onError: (error) => {
      setPending({});
      toast(errorText(error), "error");
    },
  });
  return {
    ...stored,
    ...pending,
    saving: save.isPending,
    set: (changes: Partial<CodeSettings>) => save.mutate(changes),
  };
}

export function CodesPage() {
  const { t } = useT();
  const settings = useCodeSettings();

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={t("codes.title")} intro={t("codes.intro")} />
      <div className="flex max-w-[760px] flex-col gap-5">
        <Card icon={ICONS.secret} title={t("codes.card.title")} subtitle={t("codes.card.intro")}>
          <div className="flex flex-col gap-4">
            <Toggle
              checked={settings.detect}
              disabled={settings.saving}
              onChange={(detect) => settings.set({ detect })}
              label={t("codes.detect.label")}
              description={t("codes.detect.description")}
            />
            <div className="border-t border-hairline" />
            <SettingRow
              label={t("codes.cleanup.label")}
              description={t(`codes.cleanup.description.${settings.cleanup}`)}
            >
              {() => (
                <Segmented
                  label={t("codes.cleanup.label")}
                  disabled={settings.saving}
                  value={settings.cleanup}
                  onChange={(cleanup) => settings.set({ cleanup })}
                  options={CODE_CLEANUPS.map((value) => ({ value, label: t(`codes.cleanup.options.${value}`) }))}
                />
              )}
            </SettingRow>
            <Hint className="flex gap-2.5">
              <Icon icon={ICONS.securityCheck} className="mt-0.5 size-4 shrink-0 text-pink-ink" aria-hidden />
              <span>{t("codes.signed", { header: "Verification-Info" })}</span>
            </Hint>
            <CodeChipPreview />
          </div>
        </Card>
      </div>
    </div>
  );
}
