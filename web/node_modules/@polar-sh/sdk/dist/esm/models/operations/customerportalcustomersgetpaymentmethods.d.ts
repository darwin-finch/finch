import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceUnionPaymentMethodCardPaymentMethodGeneric, ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound } from "../components/listresourceunionpaymentmethodcardpaymentmethodgeneric.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalCustomersGetPaymentMethodsSecurity = {
    customerSession: string;
};
export type CustomerPortalCustomersGetPaymentMethodsRequest = {
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
};
export type CustomerPortalCustomersGetPaymentMethodsResponse = {
    result: ListResourceUnionPaymentMethodCardPaymentMethodGeneric;
};
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsSecurity$inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersGetPaymentMethodsSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsSecurity$outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersGetPaymentMethodsSecurity$ {
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsSecurity$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsSecurity>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsSecurity$Outbound` instead. */
    type Outbound = CustomerPortalCustomersGetPaymentMethodsSecurity$Outbound;
}
export declare function customerPortalCustomersGetPaymentMethodsSecurityToJSON(customerPortalCustomersGetPaymentMethodsSecurity: CustomerPortalCustomersGetPaymentMethodsSecurity): string;
export declare function customerPortalCustomersGetPaymentMethodsSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersGetPaymentMethodsSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsRequest$inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersGetPaymentMethodsRequest$Outbound = {
    page: number;
    limit: number;
};
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsRequest$outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsRequest$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersGetPaymentMethodsRequest$ {
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsRequest$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsRequest>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsRequest$Outbound` instead. */
    type Outbound = CustomerPortalCustomersGetPaymentMethodsRequest$Outbound;
}
export declare function customerPortalCustomersGetPaymentMethodsRequestToJSON(customerPortalCustomersGetPaymentMethodsRequest: CustomerPortalCustomersGetPaymentMethodsRequest): string;
export declare function customerPortalCustomersGetPaymentMethodsRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersGetPaymentMethodsRequest, SDKValidationError>;
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsResponse$inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalCustomersGetPaymentMethodsResponse$Outbound = {
    Result: ListResourceUnionPaymentMethodCardPaymentMethodGeneric$Outbound;
};
/** @internal */
export declare const CustomerPortalCustomersGetPaymentMethodsResponse$outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsResponse$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalCustomersGetPaymentMethodsResponse$ {
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalCustomersGetPaymentMethodsResponse$Outbound, z.ZodTypeDef, CustomerPortalCustomersGetPaymentMethodsResponse>;
    /** @deprecated use `CustomerPortalCustomersGetPaymentMethodsResponse$Outbound` instead. */
    type Outbound = CustomerPortalCustomersGetPaymentMethodsResponse$Outbound;
}
export declare function customerPortalCustomersGetPaymentMethodsResponseToJSON(customerPortalCustomersGetPaymentMethodsResponse: CustomerPortalCustomersGetPaymentMethodsResponse): string;
export declare function customerPortalCustomersGetPaymentMethodsResponseFromJSON(jsonString: string): SafeParseResult<CustomerPortalCustomersGetPaymentMethodsResponse, SDKValidationError>;
//# sourceMappingURL=customerportalcustomersgetpaymentmethods.d.ts.map