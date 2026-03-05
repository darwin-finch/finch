import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Checkout, Checkout$Outbound } from "./checkout.js";
/**
 * Sent when a checkout is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookCheckoutUpdatedPayload = {
    type?: "checkout.updated" | undefined;
    /**
     * Checkout session data retrieved using an access token.
     */
    data: Checkout;
};
/** @internal */
export declare const WebhookCheckoutUpdatedPayload$inboundSchema: z.ZodType<WebhookCheckoutUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookCheckoutUpdatedPayload$Outbound = {
    type: "checkout.updated";
    data: Checkout$Outbound;
};
/** @internal */
export declare const WebhookCheckoutUpdatedPayload$outboundSchema: z.ZodType<WebhookCheckoutUpdatedPayload$Outbound, z.ZodTypeDef, WebhookCheckoutUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookCheckoutUpdatedPayload$ {
    /** @deprecated use `WebhookCheckoutUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookCheckoutUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookCheckoutUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookCheckoutUpdatedPayload$Outbound, z.ZodTypeDef, WebhookCheckoutUpdatedPayload>;
    /** @deprecated use `WebhookCheckoutUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookCheckoutUpdatedPayload$Outbound;
}
export declare function webhookCheckoutUpdatedPayloadToJSON(webhookCheckoutUpdatedPayload: WebhookCheckoutUpdatedPayload): string;
export declare function webhookCheckoutUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookCheckoutUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookcheckoutupdatedpayload.d.ts.map