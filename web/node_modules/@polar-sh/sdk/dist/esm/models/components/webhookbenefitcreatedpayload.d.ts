import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Benefit, Benefit$Outbound } from "./benefit.js";
/**
 * Sent when a new benefit is created.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookBenefitCreatedPayload = {
    type?: "benefit.created" | undefined;
    data: Benefit;
};
/** @internal */
export declare const WebhookBenefitCreatedPayload$inboundSchema: z.ZodType<WebhookBenefitCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookBenefitCreatedPayload$Outbound = {
    type: "benefit.created";
    data: Benefit$Outbound;
};
/** @internal */
export declare const WebhookBenefitCreatedPayload$outboundSchema: z.ZodType<WebhookBenefitCreatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookBenefitCreatedPayload$ {
    /** @deprecated use `WebhookBenefitCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookBenefitCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookBenefitCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookBenefitCreatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitCreatedPayload>;
    /** @deprecated use `WebhookBenefitCreatedPayload$Outbound` instead. */
    type Outbound = WebhookBenefitCreatedPayload$Outbound;
}
export declare function webhookBenefitCreatedPayloadToJSON(webhookBenefitCreatedPayload: WebhookBenefitCreatedPayload): string;
export declare function webhookBenefitCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookBenefitCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookbenefitcreatedpayload.d.ts.map