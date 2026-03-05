import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerState, CustomerState$Outbound } from "./customerstate.js";
/**
 * Sent when a customer state has changed.
 *
 * @remarks
 *
 * It's triggered when:
 *
 * * Customer is created, updated or deleted.
 * * A subscription is created or updated.
 * * A benefit is granted or revoked.
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookCustomerStateChangedPayload = {
    type?: "customer.state_changed" | undefined;
    /**
     * A customer along with additional state information:
     *
     * @remarks
     *
     * * Active subscriptions
     * * Active benefits
     */
    data: CustomerState;
};
/** @internal */
export declare const WebhookCustomerStateChangedPayload$inboundSchema: z.ZodType<WebhookCustomerStateChangedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookCustomerStateChangedPayload$Outbound = {
    type: "customer.state_changed";
    data: CustomerState$Outbound;
};
/** @internal */
export declare const WebhookCustomerStateChangedPayload$outboundSchema: z.ZodType<WebhookCustomerStateChangedPayload$Outbound, z.ZodTypeDef, WebhookCustomerStateChangedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookCustomerStateChangedPayload$ {
    /** @deprecated use `WebhookCustomerStateChangedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookCustomerStateChangedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookCustomerStateChangedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookCustomerStateChangedPayload$Outbound, z.ZodTypeDef, WebhookCustomerStateChangedPayload>;
    /** @deprecated use `WebhookCustomerStateChangedPayload$Outbound` instead. */
    type Outbound = WebhookCustomerStateChangedPayload$Outbound;
}
export declare function webhookCustomerStateChangedPayloadToJSON(webhookCustomerStateChangedPayload: WebhookCustomerStateChangedPayload): string;
export declare function webhookCustomerStateChangedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookCustomerStateChangedPayload, SDKValidationError>;
//# sourceMappingURL=webhookcustomerstatechangedpayload.d.ts.map