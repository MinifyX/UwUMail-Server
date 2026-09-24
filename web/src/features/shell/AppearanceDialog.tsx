import { Dialog } from "@/components/ui/Dialog";
import { Field, Segmented, Select } from "@/components/ui/Field";
import { LANGUAGE_NAMES, LANGUAGES, useT } from "@/i18n";
import { useBrand } from "@/state/brand";
import { useSavePrefs } from "@/features/session/session";
import { usePrefs, type LanguageSetting, type MotionSetting, type ThemeSetting, type Tone } from "@/state/prefs";

export function AppearanceDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useT();
  const prefs = usePrefs();
  const save = useSavePrefs();
  const mascot = useBrand((s) => s.mascot);

  return (
    <Dialog open={open} onClose={onClose} title={t("appearance.title")} width="sm">
      <div className="flex flex-col gap-5 px-6 pt-2 pb-6">
        <Field label={t("appearance.language.label")}>
          {(id) => (
            <Select
              id={id}
              value={prefs.language}
              onChange={(event) => save.mutate({ language: event.target.value as LanguageSetting })}
            >
              <option value="system">{t("appearance.language.system")}</option>
              {LANGUAGES.map((language) => (
                <option key={language} value={language} lang={language}>
                  {LANGUAGE_NAMES[language]}
                </option>
              ))}
            </Select>
          )}
        </Field>
        {/* Without the mascot everything is plain, so there is no tone to choose. */}
        {mascot && (
          <Field label={t("appearance.tone.label")} hint={t("appearance.tone.hint")}>
            {() => (
              <Segmented<Tone>
                label={t("appearance.tone.label")}
                value={prefs.tone}
                onChange={(tone) => save.mutate({ tone })}
                options={[
                  { value: "playful", label: t("appearance.tone.playful") },
                  { value: "neutral", label: t("appearance.tone.neutral") },
                ]}
              />
            )}
          </Field>
        )}
        <Field label={t("appearance.theme.label")}>
          {() => (
            <Segmented<ThemeSetting>
              label={t("appearance.theme.label")}
              value={prefs.theme}
              onChange={(theme) => save.mutate({ theme })}
              options={(["system", "light", "dark"] as const).map((value) => ({
                value,
                label: t(`appearance.theme.${value}`),
              }))}
            />
          )}
        </Field>
        <Field label={t("appearance.motion.label")}>
          {() => (
            <Segmented<MotionSetting>
              label={t("appearance.motion.label")}
              value={prefs.motion}
              onChange={(motion) => save.mutate({ motion })}
              options={(["system", "on", "off"] as const).map((value) => ({
                value,
                label: t(`appearance.motion.${value}`),
              }))}
            />
          )}
        </Field>
        {save.isError && (
          <p role="alert" className="text-[13px] text-danger">
            {t("appearance.saveFailed")}
          </p>
        )}
      </div>
    </Dialog>
  );
}
