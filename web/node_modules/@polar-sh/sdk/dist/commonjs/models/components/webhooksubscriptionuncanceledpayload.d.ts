import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Subscription, Subscription$Outbound } from "./subscription.js";
/**
 * Sent when a subscription is uncanceled.
 *
 * @remarks
 *
 * **Discord & Slack support:** Full
 */
export type WebhookSubscriptionUncanceledPayload = {
    type?: "subscription.uncanceled" | undefined;
    data: Subscription;
};
/** @internal */
export declare const WebhookSubscriptionUncanceledPayload$inboundSchema: z.ZodType<WebhookSubscriptionUncanceledPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookSubscriptionUncanceledPayload$Outbound = {
    type: "subscription.uncanceled";
    data: Subscription$Outbound;
};
/** @internal */
export declare const WebhookSubscriptionUncanceledPayload$outboundSchema: z.ZodType<WebhookSubscriptionUncanceledPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionUncanceledPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookSubscriptionUncanceledPayload$ {
    /** @deprecated use `WebhookSubscriptionUncanceledPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookSubscriptionUncanceledPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookSubscriptionUncanceledPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookSubscriptionUncanceledPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionUncanceledPayload>;
    /** @deprecated use `WebhookSubscriptionUncanceledPayload$Outbound` instead. */
    type Outbound = WebhookSubscriptionUncanceledPayload$Outbound;
}
export declare function webhookSubscriptionUncanceledPayloadToJSON(webhookSubscriptionUncanceledPayload: WebhookSubscriptionUncanceledPayload): string;
export declare function webhookSubscriptionUncanceledPayloadFromJSON(jsonString: string): SafeParseResult<WebhookSubscriptionUncanceledPayload, SDKValidationError>;
//# sourceMappingURL=webhooksubscriptionuncanceledpayload.d.ts.map