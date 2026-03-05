import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Refund, Refund$Outbound } from "./refund.js";
/**
 * Sent when a refund is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Full
 */
export type WebhookRefundUpdatedPayload = {
    type?: "refund.updated" | undefined;
    data: Refund;
};
/** @internal */
export declare const WebhookRefundUpdatedPayload$inboundSchema: z.ZodType<WebhookRefundUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookRefundUpdatedPayload$Outbound = {
    type: "refund.updated";
    data: Refund$Outbound;
};
/** @internal */
export declare const WebhookRefundUpdatedPayload$outboundSchema: z.ZodType<WebhookRefundUpdatedPayload$Outbound, z.ZodTypeDef, WebhookRefundUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookRefundUpdatedPayload$ {
    /** @deprecated use `WebhookRefundUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookRefundUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookRefundUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookRefundUpdatedPayload$Outbound, z.ZodTypeDef, WebhookRefundUpdatedPayload>;
    /** @deprecated use `WebhookRefundUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookRefundUpdatedPayload$Outbound;
}
export declare function webhookRefundUpdatedPayloadToJSON(webhookRefundUpdatedPayload: WebhookRefundUpdatedPayload): string;
export declare function webhookRefundUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookRefundUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookrefundupdatedpayload.d.ts.map