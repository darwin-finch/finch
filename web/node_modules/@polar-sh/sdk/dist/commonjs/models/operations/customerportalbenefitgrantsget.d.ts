import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalBenefitGrantsGetSecurity = {
    customerSession: string;
};
export type CustomerPortalBenefitGrantsGetRequest = {
    /**
     * The benefit grant ID.
     */
    id: string;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsGetSecurity$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsGetSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsGetSecurity$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsGetSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsGetSecurity$ {
    /** @deprecated use `CustomerPortalBenefitGrantsGetSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsGetSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsGetSecurity>;
    /** @deprecated use `CustomerPortalBenefitGrantsGetSecurity$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsGetSecurity$Outbound;
}
export declare function customerPortalBenefitGrantsGetSecurityToJSON(customerPortalBenefitGrantsGetSecurity: CustomerPortalBenefitGrantsGetSecurity): string;
export declare function customerPortalBenefitGrantsGetSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsGetSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsGetRequest$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsGetRequest$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsGetRequest$ {
    /** @deprecated use `CustomerPortalBenefitGrantsGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsGetRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsGetRequest>;
    /** @deprecated use `CustomerPortalBenefitGrantsGetRequest$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsGetRequest$Outbound;
}
export declare function customerPortalBenefitGrantsGetRequestToJSON(customerPortalBenefitGrantsGetRequest: CustomerPortalBenefitGrantsGetRequest): string;
export declare function customerPortalBenefitGrantsGetRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsGetRequest, SDKValidationError>;
//# sourceMappingURL=customerportalbenefitgrantsget.d.ts.map