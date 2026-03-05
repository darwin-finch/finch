import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalOrganizationsGetRequest = {
    /**
     * The organization slug.
     */
    slug: string;
};
/** @internal */
export declare const CustomerPortalOrganizationsGetRequest$inboundSchema: z.ZodType<CustomerPortalOrganizationsGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalOrganizationsGetRequest$Outbound = {
    slug: string;
};
/** @internal */
export declare const CustomerPortalOrganizationsGetRequest$outboundSchema: z.ZodType<CustomerPortalOrganizationsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalOrganizationsGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalOrganizationsGetRequest$ {
    /** @deprecated use `CustomerPortalOrganizationsGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalOrganizationsGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalOrganizationsGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalOrganizationsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalOrganizationsGetRequest>;
    /** @deprecated use `CustomerPortalOrganizationsGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalOrganizationsGetRequest$Outbound;
}
export declare function customerPortalOrganizationsGetRequestToJSON(customerPortalOrganizationsGetRequest: CustomerPortalOrganizationsGetRequest): string;
export declare function customerPortalOrganizationsGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalOrganizationsGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportalorganizationsget.d.ts.map