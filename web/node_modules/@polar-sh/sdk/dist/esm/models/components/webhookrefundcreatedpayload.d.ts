import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Refund, Refund$Outbound } from "./refund.js";
/**
 * Sent when a refund is created regardless of status.
 *
 * @remarks
 *
 * **Discord & Slack support:** Full
 */
export type WebhookRefundCreatedPayload = {
    type?: "refund.created" | undefined;
    data: Refund;
};
/** @internal */
export declare const WebhookRefundCreatedPayload$inboundSchema: z.ZodType<WebhookRefundCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookRefundCreatedPayload$Outbound = {
    type: "refund.created";
    data: Refund$Outbound;
};
/** @internal */
export declare const WebhookRefundCreatedPayload$outboundSchema: z.ZodType<WebhookRefundCreatedPayload$Outbound, z.ZodTypeDef, WebhookRefundCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookRefundCreatedPayload$ {
    /** @deprecated use `WebhookRefundCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookRefundCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookRefundCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookRefundCreatedPayload$Outbound, z.ZodTypeDef, WebhookRefundCreatedPayload>;
    /** @deprecated use `WebhookRefundCreatedPayload$Outbound` instead. */
    type Outbound = WebhookRefundCreatedPayload$Outbound;
}
export declare function webhookRefundCreatedPayloadToJSON(webhookRefundCreatedPayload: WebhookRefundCreatedPayload): string;
export declare function webhookRefundCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookRefundCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookrefundcreatedpayload.d.ts.map