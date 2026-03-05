import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalCustomersDeletePaymentMethodSecurity = {
    customerSession: string;
};
export type CustomerPortalCustomersDeletePaymentMethodRequest = {
    id: string;
};
/** @internal */
export declare const CustomerPortalCustomersDeletePaymentMethodSecurity$inboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersDeletePaymentMethodSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalCustomersDeletePaymentMethodSecurity$outboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersDeletePaymentMethodSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersDeletePaymentMethodSecurity$ {
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersDeletePaymentMethodSecurity>;
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodSecurity$Outbound` instead. */
    type Outbound = CustomerPortalCustomersDeletePaymentMethodSecurity$Outbound;
}
export declare function customerPortalCustomersDeletePaymentMethodSecurityToJSON(customerPortalCustomersDeletePaymentMethodSecurity: CustomerPortalCustomersDeletePaymentMethodSecurity): string;
export declare function customerPortalCustomersDeletePaymentMethodSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersDeletePaymentMethodSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalCustomersDeletePaymentMethodRequest$inboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersDeletePaymentMethodRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalCustomersDeletePaymentMethodRequest$outboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodRequest$Outbound, z.ZodTypeDef, CustomerPortalCustomersDeletePaymentMethodRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersDeletePaymentMethodRequest$ {
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersDeletePaymentMethodRequest$Outbound, z.ZodTypeDef, CustomerPortalCustomersDeletePaymentMethodRequest>;
    /** @deprecated use `CustomerPortalCustomersDeletePaymentMethodRequest$Outbound` instead. */
    type Outbound = CustomerPortalCustomersDeletePaymentMethodRequest$Outbound;
}
export declare function customerPortalCustomersDeletePaymentMethodRequestToJSON(customerPortalCustomersDeletePaymentMethodRequest: CustomerPortalCustomersDeletePaymentMethodRequest): string;
export declare function customerPortalCustomersDeletePaymentMethodRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersDeletePaymentMethodRequest, SDKValidationError>;
//# sourceMappingURL=customerportalcustomersdeletepaymentmethod.d.ts.map