import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CustomerSubscriptionUpdate, CustomerSubscriptionUpdate$Outbound } from "../components/customersubscriptionupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalSubscriptionsUpdateSecurity = {
    customerSession: string;
};
export type CustomerPortalSubscriptionsUpdateRequest = {
    /**
     * The subscription ID.
     */
    id: string;
    customerSubscriptionUpdate: CustomerSubscriptionUpdate;
};
/** @internal */
export declare const CustomerPortalSubscriptionsUpdateSecurity$inboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsUpdateSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsUpdateSecurity$outboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsUpdateSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsUpdateSecurity$ {
    /** @deprecated use `CustomerPortalSubscriptionsUpdateSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsUpdateSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsUpdateSecurity>;
    /** @deprecated use `CustomerPortalSubscriptionsUpdateSecurity$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsUpdateSecurity$Outbound;
}
export declare function customerPortalSubscriptionsUpdateSecurityToJSON(customerPortalSubscriptionsUpdateSecurity: CustomerPortalSubscriptionsUpdateSecurity): string;
export declare function customerPortalSubscriptionsUpdateSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsUpdateSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsUpdateRequest$inboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsUpdateRequest$Outbound = {
    id: string;
    CustomerSubscriptionUpdate: CustomerSubscriptionUpdate$Outbound;
};
/** @internal */
export declare const CustomerPortalSubscriptionsUpdateRequest$outboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsUpdateRequest$ {
    /** @deprecated use `CustomerPortalSubscriptionsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsUpdateRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsUpdateRequest>;
    /** @deprecated use `CustomerPortalSubscriptionsUpdateRequest$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsUpdateRequest$Outbound;
}
export declare function customerPortalSubscriptionsUpdateRequestToJSON(customerPortalSubscriptionsUpdateRequest: CustomerPortalSubscriptionsUpdateRequest): string;
export declare function customerPortalSubscriptionsUpdateRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=customerportalsubscriptionsupdate.d.ts.map