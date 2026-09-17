import { NyuScene } from "@/components/nyu/scenes";
import { useT } from "@/i18n";
import { Button } from "./Button";
import { Dialog } from "./Dialog";

interface ConfirmCloseSecretDialogProps {
  open: boolean;
  onClose: () => void;
  onBack: () => void;
}

/** Nyu double-checks before something that is only shown once disappears. */
export function ConfirmCloseSecretDialog({ open, onClose, onBack }: ConfirmCloseSecretDialogProps) {
  const { t } = useT();
  return (
    <Dialog open={open} onClose={onBack} width="sm">
      {open && (
        <div className="flex flex-col items-center gap-3 px-6 pt-6 pb-6 text-center">
          <NyuScene name="noPreview" className="h-auto w-[180px]" />
          <h2 className="text-lg font-bold">{t("keepSecret.title")}</h2>
          <p className="text-[13px] text-muted">{t("keepSecret.body")}</p>
          <div className="mt-1 flex flex-wrap justify-center gap-2">
            <Button variant="primary" autoFocus onClick={onBack}>
              {t("keepSecret.back")}
            </Button>
            <Button variant="danger" onClick={onClose}>
              {t("keepSecret.close")}
            </Button>
          </div>
        </div>
      )}
    </Dialog>
  );
}
