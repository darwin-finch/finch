import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitGrantWebhook, BenefitGrantWebhook$Outbound } from "./benefitgrantwebhook.js";
/**
 * Sent when a new benefit grant is created.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookBenefitGrantCreatedPayload = {
    type?: "benefit_grant.created" | undefined;
    data: BenefitGrantWebhook;
};
/** @internal */
export declare const WebhookBenefitGrantCreatedPayload$inboundSchema: z.ZodType<WebhookBenefitGrantCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookBenefitGrantCreatedPayload$Outbound = {
    type: "benefit_grant.created";
    data: BenefitGrantWebhook$Outbound;
};
/** @internal */
export declare const WebhookBenefitGrantCreatedPayload$outboundSchema: z.ZodType<WebhookBenefitGrantCreatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitGrantCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookBenefitGrantCreatedPayload$ {
    /** @deprecated use `WebhookBenefitGrantCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookBenefitGrantCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookBenefitGrantCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookBenefitGrantCreatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitGrantCreatedPayload>;
    /** @deprecated use `WebhookBenefitGrantCreatedPayload$Outbound` instead. */
    type Outbound = WebhookBenefitGrantCreatedPayload$Outbound;
}
export declare function webhookBenefitGrantCreatedPayloadToJSON(webhookBenefitGrantCreatedPayload: WebhookBenefitGrantCreatedPayload): string;
export declare function webhookBenefitGrantCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookBenefitGrantCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookbenefitgrantcreatedpayload.d.ts.map