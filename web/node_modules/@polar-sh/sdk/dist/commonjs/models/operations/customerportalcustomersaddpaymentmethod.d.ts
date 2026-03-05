import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { PaymentMethodCard, PaymentMethodCard$Outbound } from "../components/paymentmethodcard.js";
import { PaymentMethodGeneric, PaymentMethodGeneric$Outbound } from "../components/paymentmethodgeneric.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalCustomersAddPaymentMethodSecurity = {
    customerSession: string;
};
/**
 * Payment method created.
 */
export type CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod = PaymentMethodGeneric | PaymentMethodCard;
/** @internal */
export declare const CustomerPortalCustomersAddPaymentMethodSecurity$inboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersAddPaymentMethodSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalCustomersAddPaymentMethodSecurity$outboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersAddPaymentMethodSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersAddPaymentMethodSecurity$ {
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersAddPaymentMethodSecurity>;
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodSecurity$Outbound` instead. */
    type Outbound = CustomerPortalCustomersAddPaymentMethodSecurity$Outbound;
}
export declare function customerPortalCustomersAddPaymentMethodSecurityToJSON(customerPortalCustomersAddPaymentMethodSecurity: CustomerPortalCustomersAddPaymentMethodSecurity): string;
export declare function customerPortalCustomersAddPaymentMethodSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersAddPaymentMethodSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$inboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$Outbound = PaymentMethodGeneric$Outbound | PaymentMethodCard$Outbound;
/** @internal */
export declare const CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$outboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$Outbound, z.ZodTypeDef, CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$ {
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$Outbound, z.ZodTypeDef, CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod>;
    /** @deprecated use `CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$Outbound` instead. */
    type Outbound = CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod$Outbound;
}
export declare function customerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethodToJSON(customerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod: CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod): string;
export declare function customerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethodFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersAddPaymentMethodResponseCustomerPortalCustomersAddPaymentMethod, SDKValidationError>;
//# sourceMappingURL=customerportalcustomersaddpaymentmethod.d.ts.map