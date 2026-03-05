import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalCustomersUpdateSecurity = {
    customerSession: string;
};
/** @internal */
export declare const CustomerPortalCustomersUpdateSecurity$inboundSchema: z.ZodType<CustomerPortalCustomersUpdateSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersUpdateSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalCustomersUpdateSecurity$outboundSchema: z.ZodType<CustomerPortalCustomersUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersUpdateSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersUpdateSecurity$ {
    /** @deprecated use `CustomerPortalCustomersUpdateSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersUpdateSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersUpdateSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersUpdateSecurity>;
    /** @deprecated use `CustomerPortalCustomersUpdateSecurity$Outbound` instead. */
    type Outbound = CustomerPortalCustomersUpdateSecurity$Outbound;
}
export declare function customerPortalCustomersUpdateSecurityToJSON(customerPortalCustomersUpdateSecurity: CustomerPortalCustomersUpdateSecurity): string;
export declare function customerPortalCustomersUpdateSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersUpdateSecurity, SDKValidationError>;
//# sourceMappingURL=customerportalcustomersupdate.d.ts.map