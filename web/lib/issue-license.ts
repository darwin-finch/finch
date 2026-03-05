/**
 * Issues a signed FINCH-... license key.
 *
 * Mirrors the logic in scripts/issue_license.py — must stay in sync.
 * Key format: FINCH-<base64url(JSON payload)>.<base64url(Ed25519 signature)>
 */

import { createPrivateKey, sign } from "node:crypto";

export function issueKey(
  email: string,
  name: string,
  privateKeyPem: string,
  years = 1
): string {
  const today = new Date();
  const expiry = new Date(today);
  expiry.setFullYear(expiry.getFullYear() + years);

  const fmt = (d: Date) => d.toISOString().split("T")[0];

  // Must match the Python implementation exactly (no spaces in JSON)
  const payload = JSON.stringify({
    sub: email,
    name,
    tier: "commercial",
    iss: fmt(today),
    exp: fmt(expiry),
  });

  const payloadBytes = Buffer.from(payload);
  const privateKey = createPrivateKey(privateKeyPem);
  const signature = sign(null, payloadBytes, privateKey);

  const b64url = (buf: Buffer) => buf.toString("base64url");
  return `FINCH-${b64url(payloadBytes)}.${b64url(signature)}`;
}
