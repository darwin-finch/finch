import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Subscription, Subscription$Outbound } from "./subscription.js";
/**
 * Sent when a new subscription is created.
 *
 * @remarks
 *
 * When this event occurs, the subscription `status` might not be `active` yet, as we can still have to wait for the first payment to be processed.
 *
 * **Discord & Slack support:** Full
 */
export type WebhookSubscriptionCreatedPayload = {
    type?: "subscription.created" | undefined;
    data: Subscription;
};
/** @internal */
export declare const WebhookSubscriptionCreatedPayload$inboundSchema: z.ZodType<WebhookSubscriptionCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookSubscriptionCreatedPayload$Outbound = {
    type: "subscription.created";
    data: Subscription$Outbound;
};
/** @internal */
export declare const WebhookSubscriptionCreatedPayload$outboundSchema: z.ZodType<WebhookSubscriptionCreatedPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookSubscriptionCreatedPayload$ {
    /** @deprecated use `WebhookSubscriptionCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookSubscriptionCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookSubscriptionCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookSubscriptionCreatedPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionCreatedPayload>;
    /** @deprecated use `WebhookSubscriptionCreatedPayload$Outbound` instead. */
    type Outbound = WebhookSubscriptionCreatedPayload$Outbound;
}
export declare function webhookSubscriptionCreatedPayloadToJSON(webhookSubscriptionCreatedPayload: WebhookSubscriptionCreatedPayload): string;
export declare function webhookSubscriptionCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookSubscriptionCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhooksubscriptioncreatedpayload.d.ts.map