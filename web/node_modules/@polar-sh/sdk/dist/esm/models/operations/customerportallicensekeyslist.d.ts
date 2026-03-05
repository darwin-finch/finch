import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceLicenseKeyRead, ListResourceLicenseKeyRead$Outbound } from "../components/listresourcelicensekeyread.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalLicenseKeysListSecurity = {
    customerSession: string;
};
/**
 * Filter by organization ID.
 */
export type CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter = string | Array<string>;
export type CustomerPortalLicenseKeysListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by a specific benefit
     */
    benefitId?: string | null | undefined;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
};
export type CustomerPortalLicenseKeysListResponse = {
    result: ListResourceLicenseKeyRead;
};
/** @internal */
export declare const CustomerPortalLicenseKeysListSecurity$inboundSchema: z.ZodType<CustomerPortalLicenseKeysListSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysListSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalLicenseKeysListSecurity$outboundSchema: z.ZodType<CustomerPortalLicenseKeysListSecurity$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysListSecurity$ {
    /** @deprecated use `CustomerPortalLicenseKeysListSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysListSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysListSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysListSecurity$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListSecurity>;
    /** @deprecated use `CustomerPortalLicenseKeysListSecurity$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysListSecurity$Outbound;
}
export declare function customerPortalLicenseKeysListSecurityToJSON(customerPortalLicenseKeysListSecurity: CustomerPortalLicenseKeysListSecurity): string;
export declare function customerPortalLicenseKeysListSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysListSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter$Outbound;
}
export declare function customerPortalLicenseKeysListQueryParamOrganizationIDFilterToJSON(customerPortalLicenseKeysListQueryParamOrganizationIDFilter: CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter): string;
export declare function customerPortalLicenseKeysListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalLicenseKeysListRequest$inboundSchema: z.ZodType<CustomerPortalLicenseKeysListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    benefit_id?: string | null | undefined;
    page: number;
    limit: number;
};
/** @internal */
export declare const CustomerPortalLicenseKeysListRequest$outboundSchema: z.ZodType<CustomerPortalLicenseKeysListRequest$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysListRequest$ {
    /** @deprecated use `CustomerPortalLicenseKeysListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysListRequest$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListRequest>;
    /** @deprecated use `CustomerPortalLicenseKeysListRequest$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysListRequest$Outbound;
}
export declare function customerPortalLicenseKeysListRequestToJSON(customerPortalLicenseKeysListRequest: CustomerPortalLicenseKeysListRequest): string;
export declare function customerPortalLicenseKeysListRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysListRequest, SDKValidationError>;
/** @internal */
export declare const CustomerPortalLicenseKeysListResponse$inboundSchema: z.ZodType<CustomerPortalLicenseKeysListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalLicenseKeysListResponse$Outbound = {
    Result: ListResourceLicenseKeyRead$Outbound;
};
/** @internal */
export declare const CustomerPortalLicenseKeysListResponse$outboundSchema: z.ZodType<CustomerPortalLicenseKeysListResponse$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalLicenseKeysListResponse$ {
    /** @deprecated use `CustomerPortalLicenseKeysListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalLicenseKeysListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalLicenseKeysListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalLicenseKeysListResponse$Outbound, z.ZodTypeDef, CustomerPortalLicenseKeysListResponse>;
    /** @deprecated use `CustomerPortalLicenseKeysListResponse$Outbound` instead. */
    type Outbound = CustomerPortalLicenseKeysListResponse$Outbound;
}
export declare function customerPortalLicenseKeysListResponseToJSON(customerPortalLicenseKeysListResponse: CustomerPortalLicenseKeysListResponse): string;
export declare function customerPortalLicenseKeysListResponseFromJSON(jsonString: string): SafeParseResult<CustomerPortalLicenseKeysListResponse, SDKValidationError>;
//# sourceMappingURL=customerportallicensekeyslist.d.ts.map