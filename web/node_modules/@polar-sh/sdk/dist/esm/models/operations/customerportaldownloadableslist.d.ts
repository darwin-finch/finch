import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceDownloadableRead, ListResourceDownloadableRead$Outbound } from "../components/listresourcedownloadableread.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CustomerPortalDownloadablesListSecurity = {
    customerSession: string;
};
/**
 * Filter by organization ID.
 */
export type CustomerPortalDownloadablesListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by benefit ID.
 */
export type CustomerPortalDownloadablesListQueryParamBenefitIDFilter = string | Array<string>;
export type CustomerPortalDownloadablesListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by benefit ID.
     */
    benefitId?: string | Array<string> | null | undefined;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
};
export type CustomerPortalDownloadablesListResponse = {
    result: ListResourceDownloadableRead;
};
/** @internal */
export declare const CustomerPortalDownloadablesListSecurity$inboundSchema: z.ZodType<CustomerPortalDownloadablesListSecurity, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesListSecurity$Outbound = {
    customer_session: string;
};
/** @internal */
export declare const CustomerPortalDownloadablesListSecurity$outboundSchema: z.ZodType<CustomerPortalDownloadablesListSecurity$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListSecurity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesListSecurity$ {
    /** @deprecated use `CustomerPortalDownloadablesListSecurity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesListSecurity, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesListSecurity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesListSecurity$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListSecurity>;
    /** @deprecated use `CustomerPortalDownloadablesListSecurity$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesListSecurity$Outbound;
}
export declare function customerPortalDownloadablesListSecurityToJSON(customerPortalDownloadablesListSecurity: CustomerPortalDownloadablesListSecurity): string;
export declare function customerPortalDownloadablesListSecurityFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesListSecurity, SDKValidationError>;
/** @internal */
export declare const CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesListQueryParamOrganizationIDFilter$Outbound;
}
export declare function customerPortalDownloadablesListQueryParamOrganizationIDFilterToJSON(customerPortalDownloadablesListQueryParamOrganizationIDFilter: CustomerPortalDownloadablesListQueryParamOrganizationIDFilter): string;
export declare function customerPortalDownloadablesListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalDownloadablesListQueryParamBenefitIDFilter$inboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesListQueryParamBenefitIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomerPortalDownloadablesListQueryParamBenefitIDFilter$outboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListQueryParamBenefitIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesListQueryParamBenefitIDFilter$ {
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamBenefitIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamBenefitIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesListQueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListQueryParamBenefitIDFilter>;
    /** @deprecated use `CustomerPortalDownloadablesListQueryParamBenefitIDFilter$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesListQueryParamBenefitIDFilter$Outbound;
}
export declare function customerPortalDownloadablesListQueryParamBenefitIDFilterToJSON(customerPortalDownloadablesListQueryParamBenefitIDFilter: CustomerPortalDownloadablesListQueryParamBenefitIDFilter): string;
export declare function customerPortalDownloadablesListQueryParamBenefitIDFilterFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesListQueryParamBenefitIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomerPortalDownloadablesListRequest$inboundSchema: z.ZodType<CustomerPortalDownloadablesListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    benefit_id?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
};
/** @internal */
export declare const CustomerPortalDownloadablesListRequest$outboundSchema: z.ZodType<CustomerPortalDownloadablesListRequest$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesListRequest$ {
    /** @deprecated use `CustomerPortalDownloadablesListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesListRequest$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListRequest>;
    /** @deprecated use `CustomerPortalDownloadablesListRequest$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesListRequest$Outbound;
}
export declare function customerPortalDownloadablesListRequestToJSON(customerPortalDownloadablesListRequest: CustomerPortalDownloadablesListRequest): string;
export declare function customerPortalDownloadablesListRequestFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesListRequest, SDKValidationError>;
/** @internal */
export declare const CustomerPortalDownloadablesListResponse$inboundSchema: z.ZodType<CustomerPortalDownloadablesListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomerPortalDownloadablesListResponse$Outbound = {
    Result: ListResourceDownloadableRead$Outbound;
};
/** @internal */
export declare const CustomerPortalDownloadablesListResponse$outboundSchema: z.ZodType<CustomerPortalDownloadablesListResponse$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomerPortalDownloadablesListResponse$ {
    /** @deprecated use `CustomerPortalDownloadablesListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomerPortalDownloadablesListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomerPortalDownloadablesListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomerPortalDownloadablesListResponse$Outbound, z.ZodTypeDef, CustomerPortalDownloadablesListResponse>;
    /** @deprecated use `CustomerPortalDownloadablesListResponse$Outbound` instead. */
    type Outbound = CustomerPortalDownloadablesListResponse$Outbound;
}
export declare function customerPortalDownloadablesListResponseToJSON(customerPortalDownloadablesListResponse: CustomerPortalDownloadablesListResponse): string;
export declare function customerPortalDownloadablesListResponseFromJSON(jsonString: string): SafeParseResult<CustomerPortalDownloadablesListResponse, SDKValidationError>;
//# sourceMappingURL=customerportaldownloadableslist.d.ts.map