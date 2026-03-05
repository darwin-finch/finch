import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalLicenseKeysGetSecurity = {
    customerSession: string;
};
export type CustomerPortalLicenseKeysGetRequest = {
    id: string;
};
/** @internal */
export declare const CustomerPortalLicenseKeysGetSecurity$inboundSchema: z.ZodType<CustomerPortalLicenseKeysGetSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysGetSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalLicenseKeysGetSecurity$outboundSchema: z.ZodType<CustomerPortalLicenseKeysGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysGetSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysGetSecurity$ {
    /** @deprecated use `CustomerPortalLicenseKeysGetSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysGetSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysGetSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysGetSecurity>;
    /** @deprecated use `CustomerPortalLicenseKeysGetSecurity$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysGetSecurity$Outbound;
}
export declare function customerPortalLicenseKeysGetSecurityToJSON(customerPortalLicenseKeysGetSecurity: CustomerPortalLicenseKeysGetSecurity): string;
export declare function customerPortalLicenseKeysGetSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysGetSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalLicenseKeysGetRequest$inboundSchema: z.ZodType<CustomerPortalLicenseKeysGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalLicenseKeysGetRequest$outboundSchema: z.ZodType<CustomerPortalLicenseKeysGetRequest$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysGetRequest$ {
    /** @deprecated use `CustomerPortalLicenseKeysGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysGetRequest$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysGetRequest>;
    /** @deprecated use `CustomerPortalLicenseKeysGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysGetRequest$Outbound;
}
export declare function customerPortalLicenseKeysGetRequestToJSON(customerPortalLicenseKeysGetRequest: CustomerPortalLicenseKeysGetRequest): string;
export declare function customerPortalLicenseKeysGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportallicensekeysget.d.ts.map