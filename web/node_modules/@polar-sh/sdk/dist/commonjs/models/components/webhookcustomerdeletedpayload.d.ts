import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
/**
 * Sent when a customer is deleted.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookCustomerDeletedPayload = {
    type?: "customer.deleted" | undefined;
    /**
     * A customer in an organization.
     */
    data: Customer;
};
/** @internal */
export declare const WebhookCustomerDeletedPayload$inboundSchema: z.ZodType<WebhookCustomerDeletedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookCustomerDeletedPayload$Outbound = {
    type: "customer.deleted";
    data: Customer$Outbound;
};
/** @internal */
export declare const WebhookCustomerDeletedPayload$outboundSchema: z.ZodType<WebhookCustomerDeletedPayload$Outbound, z.ZodTypeDef, WebhookCustomerDeletedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookCustomerDeletedPayload$ {
    /** @deprecated use `WebhookCustomerDeletedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookCustomerDeletedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookCustomerDeletedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookCustomerDeletedPayload$Outbound, z.ZodTypeDef, WebhookCustomerDeletedPayload>;
    /** @deprecated use `WebhookCustomerDeletedPayload$Outbound` instead. */
    type Outbound = WebhookCustomerDeletedPayload$Outbound;
}
export declare function webhookCustomerDeletedPayloadToJSON(webhookCustomerDeletedPayload: WebhookCustomerDeletedPayload): string;
export declare function webhookCustomerDeletedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookCustomerDeletedPayload, SDKValidationError>;
//# sourceMappingURL=webhookcustomerdeletedpayload.d.ts.map