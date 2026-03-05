import { createHmac, timingSafeEqual } from "node:crypto";
import { type NextRequest, NextResponse } from "next/server";
import { Resend } from "resend";
import { issueKey } from "@/lib/issue-license";

/** Verify a Standard Webhooks signature (https://www.standardwebhooks.com/) */
function verifySignature(
  body: string,
  msgId: string,
  msgTimestamp: string,
  msgSignature: string,
  secret: string
): boolean {
  // Secret may be prefixed with "whsec_"
  const secretBytes = Buffer.from(secret.replace(/^whsec_/, ""), "base64");
  const signedContent = `${msgId}.${msgTimestamp}.${body}`;
  const computed = createHmac("sha256", secretBytes)
    .update(signedContent)
    .digest("base64");

  // Header may contain multiple space-separated "v1,<sig>" entries
  return msgSignature.split(" ").some((entry) => {
    const sig = entry.split(",")[1];
    if (!sig) return false;
    try {
      return timingSafeEqual(Buffer.from(computed), Buffer.from(sig));
    } catch {
      return false;
    }
  });
}

type OrderCreatedEvent = {
  type: "order.created";
  data: {
    customer: { email: string; name?: string | null };
  };
};

export async function POST(req: NextRequest) {
  const body = await req.text();

  const msgId = req.headers.get("webhook-id") ?? "";
  const msgTimestamp = req.headers.get("webhook-timestamp") ?? "";
  const msgSignature = req.headers.get("webhook-signature") ?? "";

  if (
    !verifySignature(
      body,
      msgId,
      msgTimestamp,
      msgSignature,
      process.env.POLAR_WEBHOOK_SECRET!
    )
  ) {
    return NextResponse.json({ error: "Invalid signature" }, { status: 401 });
  }

  const event = JSON.parse(body) as { type: string; data: unknown };

  if (event.type !== "order.created") {
    return NextResponse.json({ received: true });
  }

  const { customer } = (event as OrderCreatedEvent).data;
  const email = customer.email;
  const name = customer.name ?? email;
  const pem = process.env.FINCH_LICENSE_PRIVATE_KEY_PEM!;

  let key: string;
  try {
    key = issueKey(email, name, pem);
  } catch (err) {
    console.error("Failed to issue license key:", err);
    return NextResponse.json({ error: "Key issuance failed" }, { status: 500 });
  }

  const resend = new Resend(process.env.RESEND_API_KEY!);
  const { error } = await resend.emails.send({
    from: "Finch <licenses@finch.lotus.net>",
    to: email,
    subject: "Your Finch Commercial License Key",
    text: `Hi ${name},

Thanks for purchasing a Finch commercial license.

Activate it with:

    finch license activate --key ${key}

Your license is valid for one year from today.

— Shammah
`,
  });

  if (error) {
    console.error("Failed to send email:", error);
    return NextResponse.json({ error: "Email failed" }, { status: 500 });
  }

  console.log(`Issued license for ${email}`);
  return NextResponse.json({ ok: true });
}
