import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Address, Address$Outbound } from "./address.js";
export type CustomerPortalCustomerUpdate = {
    email?: string | null | undefined;
    name?: string | null | undefined;
    billingAddress?: Address | null | undefined;
    taxId?: string | null | undefined;
};
/** @internal */
export declare const CustomerPortalCustomerUpdate$inboundSchema: z.ZodType<CustomerPortalCustomerUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomerUpdate$Outbound = {
    email?: string | null | undefined;
    name?: string | null | undefined;
    billing_address?: Address$Outbound | null | undefined;
    tax_id?: string | null | undefined;
};
/** @internal */
export declare const CustomerPortalCustomerUpdate$outboundSchema: z.ZodType<CustomerPortalCustomerUpdate$Outbound, z.ZodTypeDef, CustomerPortalCustomerUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomerUpdate$ {
    /** @deprecated use `CustomerPortalCustomerUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomerUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomerUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomerUpdate$Outbound, z.ZodTypeDef, CustomerPortalCustomerUpdate>;
    /** @deprecated use `CustomerPortalCustomerUpdate$Outbound` instead. */
    type Outbound = CustomerPortalCustomerUpdate$Outbound;
}
export declare function customerPortalCustomerUpdateToJSON(customerPortalCustomerUpdate: CustomerPortalCustomerUpdate): string;
export declare function customerPortalCustomerUpdateFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomerUpdate, SDKValidationError>;
//# sourceMappingURL=customerportalcustomerupdate.d.ts.map