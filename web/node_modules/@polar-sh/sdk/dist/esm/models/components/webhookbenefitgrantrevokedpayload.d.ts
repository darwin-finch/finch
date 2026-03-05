import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGrantWebhook, BenefitGrantWebhook$Outbound } from "./benefitgrantwebhook.js";
/**
 * Sent when a new benefit grant is revoked.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookBenefitGrantRevokedPayload = {
    type?: "benefit_grant.revoked" | undefined;
    data: BenefitGrantWebhook;
};
/** @internal */
export declare const WebhookBenefitGrantRevokedPayload$inboundSchema: z.ZodType<WebhookBenefitGrantRevokedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookBenefitGrantRevokedPayload$Outbound = {
    type: "benefit_grant.revoked";
    data: BenefitGrantWebhook$Outbound;
};
/** @internal */
export declare const WebhookBenefitGrantRevokedPayload$outboundSchema: z.ZodType<WebhookBenefitGrantRevokedPayload$Outbound, z.ZodTypeDef, WebhookBenefitGrantRevokedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookBenefitGrantRevokedPayload$ {
    /** @deprecated use `WebhookBenefitGrantRevokedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookBenefitGrantRevokedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookBenefitGrantRevokedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookBenefitGrantRevokedPayload$Outbound, z.ZodTypeDef, WebhookBenefitGrantRevokedPayload>;
    /** @deprecated use `WebhookBenefitGrantRevokedPayload$Outbound` instead. */
    type Outbound = WebhookBenefitGrantRevokedPayload$Outbound;
}
export declare function webhookBenefitGrantRevokedPayloadToJSON(webhookBenefitGrantRevokedPayload: WebhookBenefitGrantRevokedPayload): string;
export declare function webhookBenefitGrantRevokedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookBenefitGrantRevokedPayload, SDKValidationError>;
//# sourceMappingURL=webhookbenefitgrantrevokedpayload.d.ts.map