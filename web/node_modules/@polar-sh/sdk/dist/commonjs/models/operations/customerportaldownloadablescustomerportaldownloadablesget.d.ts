import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest = {
    token: string;
};
/** @internal */
export declare const CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$inboundSchema: z.ZodType<CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$Outbound = {
    token: string;
};
/** @internal */
export declare const CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$outboundSchema: z.ZodType<CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$ {
    /** @deprecated use `CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest>;
    /** @deprecated use `CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest$Outbound;
}
export declare function customerPortalDownloadablesCustomerPortalDownloadablesGetRequestToJSON(customerPortalDownloadablesCustomerPortalDownloadablesGetRequest: CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest): string;
export declare function customerPortalDownloadablesCustomerPortalDownloadablesGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesCustomerPortalDownloadablesGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportaldownloadablescustomerportaldownloadablesget.d.ts.map