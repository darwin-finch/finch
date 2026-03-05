import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalSubscriptionsCancelSecurity = {
    customerSession: string;
};
export type CustomerPortalSubscriptionsCancelRequest = {
    /**
     * The subscription ID.
     */
    id: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsCancelSecurity$inboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsCancelSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsCancelSecurity$outboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsCancelSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsCancelSecurity$ {
    /** @deprecated use `CustomerPortalSubscriptionsCancelSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsCancelSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsCancelSecurity>;
    /** @deprecated use `CustomerPortalSubscriptionsCancelSecurity$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsCancelSecurity$Outbound;
}
export declare function customerPortalSubscriptionsCancelSecurityToJSON(customerPortalSubscriptionsCancelSecurity: CustomerPortalSubscriptionsCancelSecurity): string;
export declare function customerPortalSubscriptionsCancelSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsCancelSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsCancelRequest$inboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsCancelRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsCancelRequest$outboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsCancelRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsCancelRequest$ {
    /** @deprecated use `CustomerPortalSubscriptionsCancelRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsCancelRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsCancelRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsCancelRequest>;
    /** @deprecated use `CustomerPortalSubscriptionsCancelRequest$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsCancelRequest$Outbound;
}
export declare function customerPortalSubscriptionsCancelRequestToJSON(customerPortalSubscriptionsCancelRequest: CustomerPortalSubscriptionsCancelRequest): string;
export declare function customerPortalSubscriptionsCancelRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsCancelRequest, SDKValidationError>;
//# sourceMappingURL=customerportalsubscriptionscancel.d.ts.map