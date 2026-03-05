import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalOrdersInvoiceSecurity = {
    customerSession: string;
};
export type CustomerPortalOrdersInvoiceRequest = {
    /**
     * The order ID.
     */
    id: string;
};
/** @internal */
export declare const CustomerPortalOrdersInvoiceSecurity$inboundSchema: z.ZodType<CustomerPortalOrdersInvoiceSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOrdersInvoiceSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalOrdersInvoiceSecurity$outboundSchema: z.ZodType<CustomerPortalOrdersInvoiceSecurity$Outbound, z.ZodTypeDef, CustomerPortalOrdersInvoiceSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOrdersInvoiceSecurity$ {
    /** @deprecated use `CustomerPortalOrdersInvoiceSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOrdersInvoiceSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOrdersInvoiceSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOrdersInvoiceSecurity$Outbound, z.ZodTypeDef, CustomerPortalOrdersInvoiceSecurity>;
    /** @deprecated use `CustomerPortalOrdersInvoiceSecurity$Outbound` instead. */
    type Outbound = CustomerPortalOrdersInvoiceSecurity$Outbound;
}
export declare function customerPortalOrdersInvoiceSecurityToJSON(customerPortalOrdersInvoiceSecurity: CustomerPortalOrdersInvoiceSecurity): string;
export declare function customerPortalOrdersInvoiceSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalOrdersInvoiceSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalOrdersInvoiceRequest$inboundSchema: z.ZodType<CustomerPortalOrdersInvoiceRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOrdersInvoiceRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalOrdersInvoiceRequest$outboundSchema: z.ZodType<CustomerPortalOrdersInvoiceRequest$Outbound, z.ZodTypeDef, CustomerPortalOrdersInvoiceRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOrdersInvoiceRequest$ {
    /** @deprecated use `CustomerPortalOrdersInvoiceRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOrdersInvoiceRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOrdersInvoiceRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOrdersInvoiceRequest$Outbound, z.ZodTypeDef, CustomerPortalOrdersInvoiceRequest>;
    /** @deprecated use `CustomerPortalOrdersInvoiceRequest$Outbound` instead. */
    type Outbound = CustomerPortalOrdersInvoiceRequest$Outbound;
}
export declare function customerPortalOrdersInvoiceRequestToJSON(customerPortalOrdersInvoiceRequest: CustomerPortalOrdersInvoiceRequest): string;
export declare function customerPortalOrdersInvoiceRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalOrdersInvoiceRequest, SDKValidationError>;
//# sourceMappingURL=customerportalordersinvoice.d.ts.map