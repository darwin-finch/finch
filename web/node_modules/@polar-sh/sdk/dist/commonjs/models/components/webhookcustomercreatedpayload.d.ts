import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
/**
 * Sent when a new customer is created.
 *
 * @remarks
 *
 * A customer can be created:
 *
 * * After a successful checkout.
 * * Programmatically via the API.
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookCustomerCreatedPayload = {
    type?: "customer.created" | undefined;
    /**
     * A customer in an organization.
     */
    data: Customer;
};
/** @internal */
export declare const WebhookCustomerCreatedPayload$inboundSchema: z.ZodType<WebhookCustomerCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookCustomerCreatedPayload$Outbound = {
    type: "customer.created";
    data: Customer$Outbound;
};
/** @internal */
export declare const WebhookCustomerCreatedPayload$outboundSchema: z.ZodType<WebhookCustomerCreatedPayload$Outbound, z.ZodTypeDef, WebhookCustomerCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookCustomerCreatedPayload$ {
    /** @deprecated use `WebhookCustomerCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookCustomerCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookCustomerCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookCustomerCreatedPayload$Outbound, z.ZodTypeDef, WebhookCustomerCreatedPayload>;
    /** @deprecated use `WebhookCustomerCreatedPayload$Outbound` instead. */
    type Outbound = WebhookCustomerCreatedPayload$Outbound;
}
export declare function webhookCustomerCreatedPayloadToJSON(webhookCustomerCreatedPayload: WebhookCustomerCreatedPayload): string;
export declare function webhookCustomerCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookCustomerCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookcustomercreatedpayload.d.ts.map