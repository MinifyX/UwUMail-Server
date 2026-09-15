/** Passkeys in the browser: turns the server's JSON options into WebAuthn calls and the answers back into JSON. */

function fromBase64Url(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

function toBase64Url(buffer: ArrayBuffer): string {
  let binary = "";
  for (const byte of new Uint8Array(buffer)) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

interface CredentialDescriptorJson {
  type: "public-key";
  id: string;
}

export interface CreationOptionsJson {
  challenge: string;
  rp: { id: string; name: string };
  user: { id: string; name: string; displayName: string };
  pubKeyCredParams: { type: "public-key"; alg: number }[];
  timeout: number;
  attestation: AttestationConveyancePreference;
  authenticatorSelection: AuthenticatorSelectionCriteria;
  excludeCredentials: CredentialDescriptorJson[];
}

export interface RequestOptionsJson {
  challenge: string;
  rpId: string;
  timeout: number;
  userVerification: UserVerificationRequirement;
  allowCredentials: CredentialDescriptorJson[];
}

const descriptors = (list: CredentialDescriptorJson[]): PublicKeyCredentialDescriptor[] =>
  list.map((item) => ({ type: item.type, id: fromBase64Url(item.id) }));

/**
 * Whether passkeys can work on this page: the browser supports them and the page runs under the
 * server's own name (passkeys belong to a host name, not to an IP address or another proxy name).
 */
export function passkeysAvailable(hostname: string): boolean {
  return typeof window !== "undefined" && "PublicKeyCredential" in window && window.location.hostname === hostname;
}

export async function createPasskey(options: CreationOptionsJson) {
  const credential = (await navigator.credentials.create({
    publicKey: {
      challenge: fromBase64Url(options.challenge),
      rp: options.rp,
      user: { ...options.user, id: fromBase64Url(options.user.id) },
      pubKeyCredParams: options.pubKeyCredParams,
      timeout: options.timeout,
      attestation: options.attestation,
      authenticatorSelection: options.authenticatorSelection,
      excludeCredentials: descriptors(options.excludeCredentials),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("No passkey was created", "NotAllowedError");
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    clientDataJson: toBase64Url(response.clientDataJSON),
    attestationObject: toBase64Url(response.attestationObject),
  };
}

export async function assertPasskey(options: RequestOptionsJson) {
  const credential = (await navigator.credentials.get({
    publicKey: {
      challenge: fromBase64Url(options.challenge),
      rpId: options.rpId,
      timeout: options.timeout,
      userVerification: options.userVerification,
      allowCredentials: descriptors(options.allowCredentials),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new DOMException("No passkey was used", "NotAllowedError");
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    id: toBase64Url(credential.rawId),
    clientDataJson: toBase64Url(response.clientDataJSON),
    authenticatorData: toBase64Url(response.authenticatorData),
    signature: toBase64Url(response.signature),
  };
}

/** Someone closed the browser's passkey dialog: not an error worth a red message. */
export const wasCancelled = (error: unknown) =>
  error instanceof DOMException && (error.name === "NotAllowedError" || error.name === "AbortError");
