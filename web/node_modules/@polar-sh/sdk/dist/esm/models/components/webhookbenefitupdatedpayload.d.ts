import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Benefit, Benefit$Outbound } from "./benefit.js";
/**
 * Sent when a benefit is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookBenefitUpdatedPayload = {
    type?: "benefit.updated" | undefined;
    data: Benefit;
};
/** @internal */
export declare const WebhookBenefitUpdatedPayload$inboundSchema: z.ZodType<WebhookBenefitUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookBenefitUpdatedPayload$Outbound = {
    type: "benefit.updated";
    data: Benefit$Outbound;
};
/** @internal */
export declare const WebhookBenefitUpdatedPayload$outboundSchema: z.ZodType<WebhookBenefitUpdatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookBenefitUpdatedPayload$ {
    /** @deprecated use `WebhookBenefitUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookBenefitUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookBenefitUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookBenefitUpdatedPayload$Outbound, z.ZodTypeDef, WebhookBenefitUpdatedPayload>;
    /** @deprecated use `WebhookBenefitUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookBenefitUpdatedPayload$Outbound;
}
export declare function webhookBenefitUpdatedPayloadToJSON(webhookBenefitUpdatedPayload: WebhookBenefitUpdatedPayload): string;
export declare function webhookBenefitUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookBenefitUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookbenefitupdatedpayload.d.ts.map