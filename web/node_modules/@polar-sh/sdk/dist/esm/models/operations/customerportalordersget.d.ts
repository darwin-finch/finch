import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalOrdersGetSecurity = {
    customerSession: string;
};
export type CustomerPortalOrdersGetRequest = {
    /**
     * The order ID.
     */
    id: string;
};
/** @internal */
export declare const CustomerPortalOrdersGetSecurity$inboundSchema: z.ZodType<CustomerPortalOrdersGetSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOrdersGetSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalOrdersGetSecurity$outboundSchema: z.ZodType<CustomerPortalOrdersGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalOrdersGetSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOrdersGetSecurity$ {
    /** @deprecated use `CustomerPortalOrdersGetSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOrdersGetSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOrdersGetSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOrdersGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalOrdersGetSecurity>;
    /** @deprecated use `CustomerPortalOrdersGetSecurity$Outbound` instead. */
    type Outbound = CustomerPortalOrdersGetSecurity$Outbound;
}
export declare function customerPortalOrdersGetSecurityToJSON(customerPortalOrdersGetSecurity: CustomerPortalOrdersGetSecurity): string;
export declare function customerPortalOrdersGetSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalOrdersGetSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalOrdersGetRequest$inboundSchema: z.ZodType<CustomerPortalOrdersGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOrdersGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalOrdersGetRequest$outboundSchema: z.ZodType<CustomerPortalOrdersGetRequest$Outbound, z.ZodTypeDef, CustomerPortalOrdersGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOrdersGetRequest$ {
    /** @deprecated use `CustomerPortalOrdersGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOrdersGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOrdersGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOrdersGetRequest$Outbound, z.ZodTypeDef, CustomerPortalOrdersGetRequest>;
    /** @deprecated use `CustomerPortalOrdersGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalOrdersGetRequest$Outbound;
}
export declare function customerPortalOrdersGetRequestToJSON(customerPortalOrdersGetRequest: CustomerPortalOrdersGetRequest): string;
export declare function customerPortalOrdersGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalOrdersGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportalordersget.d.ts.map