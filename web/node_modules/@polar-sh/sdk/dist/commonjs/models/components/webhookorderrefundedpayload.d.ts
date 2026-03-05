import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Order, Order$Outbound } from "./order.js";
/**
 * Sent when an order is fully or partially refunded.
 *
 * @remarks
 *
 * **Discord & Slack support:** Full
 */
export type WebhookOrderRefundedPayload = {
    type?: "order.refunded" | undefined;
    data: Order;
};
/** @internal */
export declare const WebhookOrderRefundedPayload$inboundSchema: z.ZodType<WebhookOrderRefundedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookOrderRefundedPayload$Outbound = {
    type: "order.refunded";
    data: Order$Outbound;
};
/** @internal */
export declare const WebhookOrderRefundedPayload$outboundSchema: z.ZodType<WebhookOrderRefundedPayload$Outbound, z.ZodTypeDef, WebhookOrderRefundedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookOrderRefundedPayload$ {
    /** @deprecated use `WebhookOrderRefundedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookOrderRefundedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookOrderRefundedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookOrderRefundedPayload$Outbound, z.ZodTypeDef, WebhookOrderRefundedPayload>;
    /** @deprecated use `WebhookOrderRefundedPayload$Outbound` instead. */
    type Outbound = WebhookOrderRefundedPayload$Outbound;
}
export declare function webhookOrderRefundedPayloadToJSON(webhookOrderRefundedPayload: WebhookOrderRefundedPayload): string;
export declare function webhookOrderRefundedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookOrderRefundedPayload, SDKValidationError>;
//# sourceMappingURL=webhookorderrefundedpayload.d.ts.map