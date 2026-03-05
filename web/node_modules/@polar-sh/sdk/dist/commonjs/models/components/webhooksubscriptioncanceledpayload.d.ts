import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Subscription, Subscription$Outbound } from "./subscription.js";
/**
 * Sent when a subscription is canceled.
 *
 * @remarks
 * Customers might still have access until the end of the current period.
 *
 * **Discord & Slack support:** Full
 */
export type WebhookSubscriptionCanceledPayload = {
    type?: "subscription.canceled" | undefined;
    data: Subscription;
};
/** @internal */
export declare const WebhookSubscriptionCanceledPayload$inboundSchema: z.ZodType<WebhookSubscriptionCanceledPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookSubscriptionCanceledPayload$Outbound = {
    type: "subscription.canceled";
    data: Subscription$Outbound;
};
/** @internal */
export declare const WebhookSubscriptionCanceledPayload$outboundSchema: z.ZodType<WebhookSubscriptionCanceledPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionCanceledPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookSubscriptionCanceledPayload$ {
    /** @deprecated use `WebhookSubscriptionCanceledPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookSubscriptionCanceledPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookSubscriptionCanceledPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookSubscriptionCanceledPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionCanceledPayload>;
    /** @deprecated use `WebhookSubscriptionCanceledPayload$Outbound` instead. */
    type Outbound = WebhookSubscriptionCanceledPayload$Outbound;
}
export declare function webhookSubscriptionCanceledPayloadToJSON(webhookSubscriptionCanceledPayload: WebhookSubscriptionCanceledPayload): string;
export declare function webhookSubscriptionCanceledPayloadFromJSON(jsonString: string): SafeParseResult<WebhookSubscriptionCanceledPayload, SDKValidationError>;
//# sourceMappingURL=webhooksubscriptioncanceledpayload.d.ts.map