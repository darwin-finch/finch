import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CustomerCancellationReason } from "./customercancellationreason.js";
export type CustomerSubscriptionCancel = {
    /**
     * Cancel an active subscription once the current period ends.
     *
     * @remarks
     *
     * Or uncancel a subscription currently set to be revoked at period end.
     */
    cancelAtPeriodEnd?: boolean | null | undefined;
    /**
     * Customers reason for cancellation.
     *
     * @remarks
     *
     * * `too_expensive`: Too expensive for the customer.
     * * `missing_features`: Customer is missing certain features.
     * * `switched_service`: Customer switched to another service.
     * * `unused`: Customer is not using it enough.
     * * `customer_service`: Customer is not satisfied with the customer service.
     * * `low_quality`: Customer is unhappy with the quality.
     * * `too_complex`: Customer considers the service too complicated.
     * * `other`: Other reason(s).
     */
    cancellationReason?: CustomerCancellationReason | null | undefined;
    /**
     * Customer feedback and why they decided to cancel.
     */
    cancellationComment?: string | null | undefined;
};
/** @internal */
export declare const CustomerSubscriptionCancel$inboundSchema: z.ZodType<CustomerSubscriptionCancel, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerSubscriptionCancel$Outbound = {
    cancel_at_period_end?: boolean | null | undefined;
    cancellation_reason?: string | null | undefined;
    cancellation_comment?: string | null | undefined;
};
/** @internal */
export declare const CustomerSubscriptionCancel$outboundSchema: z.ZodType<CustomerSubscriptionCancel$Outbound, z.ZodTypeDef, CustomerSubscriptionCancel>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerSubscriptionCancel$ {
    /** @deprecated use `CustomerSubscriptionCancel$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerSubscriptionCancel, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerSubscriptionCancel$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerSubscriptionCancel$Outbound, z.ZodTypeDef, CustomerSubscriptionCancel>;
    /** @deprecated use `CustomerSubscriptionCancel$Outbound` instead. */
    type Outbound = CustomerSubscriptionCancel$Outbound;
}
export declare function customerSubscriptionCancelToJSON(customerSubscriptionCancel: CustomerSubscriptionCancel): string;
export declare function customerSubscriptionCancelFromJSON(jsonString: string): SafeParseResult<CustomerSubscriptionCancel, SDKValidationError>;
//# sourceMappingURL=customersubscriptioncancel.d.ts.map