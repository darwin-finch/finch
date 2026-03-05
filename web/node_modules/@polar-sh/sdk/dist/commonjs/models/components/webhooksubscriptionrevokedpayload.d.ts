import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Subscription, Subscription$Outbound } from "./subscription.js";
/**
 * Sent when a subscription is revoked, the user looses access immediately.
 *
 * @remarks
 * Happens when the subscription is canceled, or payment is past due.
 *
 * **Discord & Slack support:** Full
 */
export type WebhookSubscriptionRevokedPayload = {
    type?: "subscription.revoked" | undefined;
    data: Subscription;
};
/** @internal */
export declare const WebhookSubscriptionRevokedPayload$inboundSchema: z.ZodType<WebhookSubscriptionRevokedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookSubscriptionRevokedPayload$Outbound = {
    type: "subscription.revoked";
    data: Subscription$Outbound;
};
/** @internal */
export declare const WebhookSubscriptionRevokedPayload$outboundSchema: z.ZodType<WebhookSubscriptionRevokedPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionRevokedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookSubscriptionRevokedPayload$ {
    /** @deprecated use `WebhookSubscriptionRevokedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookSubscriptionRevokedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookSubscriptionRevokedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookSubscriptionRevokedPayload$Outbound, z.ZodTypeDef, WebhookSubscriptionRevokedPayload>;
    /** @deprecated use `WebhookSubscriptionRevokedPayload$Outbound` instead. */
    type Outbound = WebhookSubscriptionRevokedPayload$Outbound;
}
export declare function webhookSubscriptionRevokedPayloadToJSON(webhookSubscriptionRevokedPayload: WebhookSubscriptionRevokedPayload): string;
export declare function webhookSubscriptionRevokedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookSubscriptionRevokedPayload, SDKValidationError>;
//# sourceMappingURL=webhooksubscriptionrevokedpayload.d.ts.map