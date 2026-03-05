import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalSubscriptionsGetSecurity = {
    customerSession: string;
};
export type CustomerPortalSubscriptionsGetRequest = {
    /**
     * The subscription ID.
     */
    id: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsGetSecurity$inboundSchema: z.ZodType<CustomerPortalSubscriptionsGetSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsGetSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsGetSecurity$outboundSchema: z.ZodType<CustomerPortalSubscriptionsGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsGetSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsGetSecurity$ {
    /** @deprecated use `CustomerPortalSubscriptionsGetSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsGetSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsGetSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsGetSecurity>;
    /** @deprecated use `CustomerPortalSubscriptionsGetSecurity$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsGetSecurity$Outbound;
}
export declare function customerPortalSubscriptionsGetSecurityToJSON(customerPortalSubscriptionsGetSecurity: CustomerPortalSubscriptionsGetSecurity): string;
export declare function customerPortalSubscriptionsGetSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsGetSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalSubscriptionsGetRequest$inboundSchema: z.ZodType<CustomerPortalSubscriptionsGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalSubscriptionsGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalSubscriptionsGetRequest$outboundSchema: z.ZodType<CustomerPortalSubscriptionsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalSubscriptionsGetRequest$ {
    /** @deprecated use `CustomerPortalSubscriptionsGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalSubscriptionsGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalSubscriptionsGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalSubscriptionsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalSubscriptionsGetRequest>;
    /** @deprecated use `CustomerPortalSubscriptionsGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalSubscriptionsGetRequest$Outbound;
}
export declare function customerPortalSubscriptionsGetRequestToJSON(customerPortalSubscriptionsGetRequest: CustomerPortalSubscriptionsGetRequest): string;
export declare function customerPortalSubscriptionsGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalSubscriptionsGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportalsubscriptionsget.d.ts.map