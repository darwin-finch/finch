import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
/**
 * Sent when a customer is updated.
 *
 * @remarks
 *
 * This event is fired when the customer details are updated.
 *
 * If you want to be notified when a customer subscription or benefit state changes, you should listen to the `customer_state_changed` event.
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookCustomerUpdatedPayload = {
    type?: "customer.updated" | undefined;
    /**
     * A customer in an organization.
     */
    data: Customer;
};
/** @internal */
export declare const WebhookCustomerUpdatedPayload$inboundSchema: z.ZodType<WebhookCustomerUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookCustomerUpdatedPayload$Outbound = {
    type: "customer.updated";
    data: Customer$Outbound;
};
/** @internal */
export declare const WebhookCustomerUpdatedPayload$outboundSchema: z.ZodType<WebhookCustomerUpdatedPayload$Outbound, z.ZodTypeDef, WebhookCustomerUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookCustomerUpdatedPayload$ {
    /** @deprecated use `WebhookCustomerUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookCustomerUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookCustomerUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookCustomerUpdatedPayload$Outbound, z.ZodTypeDef, WebhookCustomerUpdatedPayload>;
    /** @deprecated use `WebhookCustomerUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookCustomerUpdatedPayload$Outbound;
}
export declare function webhookCustomerUpdatedPayloadToJSON(webhookCustomerUpdatedPayload: WebhookCustomerUpdatedPayload): string;
export declare function webhookCustomerUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookCustomerUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookcustomerupdatedpayload.d.ts.map