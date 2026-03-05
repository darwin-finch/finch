import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalCustomersGetSecurity = {
    customerSession: string;
};
/** @internal */
export declare const CustomerPortalCustomersGetSecurity$inboundSchema: z.ZodType<CustomerPortalCustomersGetSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersGetSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalCustomersGetSecurity$outboundSchema: z.ZodType<CustomerPortalCustomersGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersGetSecurity$ {
    /** @deprecated use `CustomerPortalCustomersGetSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersGetSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersGetSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetSecurity>;
    /** @deprecated use `CustomerPortalCustomersGetSecurity$Outbound` instead. */
    type Outbound = CustomerPortalCustomersGetSecurity$Outbound;
}
export declare function customerPortalCustomersGetSecurityToJSON(customerPortalCustomersGetSecurity: CustomerPortalCustomersGetSecurity): string;
export declare function customerPortalCustomersGetSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersGetSecurity, SDKValidationError>;
//# sourceMappingURL=customerportalcustomersget.d.ts.map