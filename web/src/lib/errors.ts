import { useT } from "@/i18n";
import { ApiError } from "./api";

const KNOWN = [
  "lastAdmin",
  "notYourself",
  "weakPassword",
  "confirmationMismatch",
  "conflict",
  "invalid",
  "notFound",
  "linkInvalid",
  "forbidden",
  "offline",
  "internal",
  "domainInUse",
  "keyActive",
  "noPendingKeys",
  "keysNotPublished",
  "dnsUnavailable",
  "settingLocked",
  "settingsInvalid",
  "codeInvalid",
  "loginExpired",
  "confirmPassword",
  "wrongPassword",
  "samePassword",
  "tooManyAttempts",
  "totpActive",
  "totpNotStarted",
  "noSecondFactor",
  "tooManyAppPasswords",
  "passkeyInvalid",
  "passkeyExpired",
  "passkeyKnown",
  "forwardToSelf",
  "forwardingBlocked",
  "tooManyForwardTargets",
  "vacationDates",
  "vacationText",
  "aliasInvalid",
  "aliasReserved",
  "aliasDomain",
  "addressTaken",
  "aliasLimit",
  "aliasNotYours",
  "addressReserved",
  "notEmptiable",
  "forwardingThrottled",
  "setupCodeInvalid",
  "setupDone",
  "testMailFailed",
  "cloudflareFailed",
];

/** Turns an API error into a sentence for the person in front of the screen. */
export function useErrorText() {
  const { t } = useT();
  return (error: unknown): string => {
    if (!(error instanceof ApiError)) return t("errors.codes.internal");
    const code = KNOWN.includes(error.code) ? error.code : "internal";
    return t(`errors.codes.${code}`, { detail: error.message });
  };
}
