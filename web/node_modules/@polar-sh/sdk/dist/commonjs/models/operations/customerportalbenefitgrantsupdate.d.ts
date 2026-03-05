import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CustomerBenefitGrantUpdate, CustomerBenefitGrantUpdate$Outbound } from "../components/customerbenefitgrantupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalBenefitGrantsUpdateSecurity = {
    customerSession: string;
};
export type CustomerPortalBenefitGrantsUpdateRequest = {
    /**
     * The benefit grant ID.
     */
    id: string;
    customerBenefitGrantUpdate: CustomerBenefitGrantUpdate;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsUpdateSecurity$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsUpdateSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsUpdateSecurity$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsUpdateSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsUpdateSecurity$ {
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateSecurity$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsUpdateSecurity>;
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateSecurity$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsUpdateSecurity$Outbound;
}
export declare function customerPortalBenefitGrantsUpdateSecurityToJSON(customerPortalBenefitGrantsUpdateSecurity: CustomerPortalBenefitGrantsUpdateSecurity): string;
export declare function customerPortalBenefitGrantsUpdateSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsUpdateSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalBenefitGrantsUpdateRequest$inboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalBenefitGrantsUpdateRequest$Outbound = {
    id: string;
    CustomerBenefitGrantUpdate: CustomerBenefitGrantUpdate$Outbound;
};
/** @internal */
export declare const CustomerPortalBenefitGrantsUpdateRequest$outboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalBenefitGrantsUpdateRequest$ {
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalBenefitGrantsUpdateRequest$Outbound, z.ZodTypeDef, CustomerPortalBenefitGrantsUpdateRequest>;
    /** @deprecated use `CustomerPortalBenefitGrantsUpdateRequest$Outbound` instead. */
    type Outbound = CustomerPortalBenefitGrantsUpdateRequest$Outbound;
}
export declare function customerPortalBenefitGrantsUpdateRequestToJSON(customerPortalBenefitGrantsUpdateRequest: CustomerPortalBenefitGrantsUpdateRequest): string;
export declare function customerPortalBenefitGrantsUpdateRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalBenefitGrantsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=customerportalbenefitgrantsupdate.d.ts.map