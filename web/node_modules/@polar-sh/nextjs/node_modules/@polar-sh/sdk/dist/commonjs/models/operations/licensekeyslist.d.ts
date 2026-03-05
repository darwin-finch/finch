import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceLicenseKeyRead, ListResourceLicenseKeyRead$Outbound } from "../components/listresourcelicensekeyread.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type LicenseKeysListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by benefit ID.
 */
export type QueryParamBenefitIDFilter = string | Array<string>;
export type LicenseKeysListRequest = {
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
export type LicenseKeysListResponse = {
    result: ListResourceLicenseKeyRead;
};
/** @internal */
export declare const LicenseKeysListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<LicenseKeysListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type LicenseKeysListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const LicenseKeysListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<LicenseKeysListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, LicenseKeysListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace LicenseKeysListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `LicenseKeysListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<LicenseKeysListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `LicenseKeysListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<LicenseKeysListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, LicenseKeysListQueryParamOrganizationIDFilter>;
    /** @deprecated use `LicenseKeysListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = LicenseKeysListQueryParamOrganizationIDFilter$Outbound;
}
export declare function licenseKeysListQueryParamOrganizationIDFilterToJSON(licenseKeysListQueryParamOrganizationIDFilter: LicenseKeysListQueryParamOrganizationIDFilter): string;
export declare function licenseKeysListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<LicenseKeysListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const QueryParamBenefitIDFilter$inboundSchema: z.ZodType<QueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamBenefitIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamBenefitIDFilter$outboundSchema: z.ZodType<QueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, QueryParamBenefitIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamBenefitIDFilter$ {
    /** @deprecated use `QueryParamBenefitIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamBenefitIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamBenefitIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamBenefitIDFilter$Outbound, z.ZodTypeDef, QueryParamBenefitIDFilter>;
    /** @deprecated use `QueryParamBenefitIDFilter$Outbound` instead. */
    type Outbound = QueryParamBenefitIDFilter$Outbound;
}
export declare function queryParamBenefitIDFilterToJSON(queryParamBenefitIDFilter: QueryParamBenefitIDFilter): string;
export declare function queryParamBenefitIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamBenefitIDFilter, SDKValidationError>;
/** @internal */
export declare const LicenseKeysListRequest$inboundSchema: z.ZodType<LicenseKeysListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type LicenseKeysListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    benefit_id?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
};
/** @internal */
export declare const LicenseKeysListRequest$outboundSchema: z.ZodType<LicenseKeysListRequest$Outbound, z.ZodTypeDef, LicenseKeysListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace LicenseKeysListRequest$ {
    /** @deprecated use `LicenseKeysListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<LicenseKeysListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `LicenseKeysListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<LicenseKeysListRequest$Outbound, z.ZodTypeDef, LicenseKeysListRequest>;
    /** @deprecated use `LicenseKeysListRequest$Outbound` instead. */
    type Outbound = LicenseKeysListRequest$Outbound;
}
export declare function licenseKeysListRequestToJSON(licenseKeysListRequest: LicenseKeysListRequest): string;
export declare function licenseKeysListRequestFromJSON(jsonString: string): SafeParseResult<LicenseKeysListRequest, SDKValidationError>;
/** @internal */
export declare const LicenseKeysListResponse$inboundSchema: z.ZodType<LicenseKeysListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type LicenseKeysListResponse$Outbound = {
    Result: ListResourceLicenseKeyRead$Outbound;
};
/** @internal */
export declare const LicenseKeysListResponse$outboundSchema: z.ZodType<LicenseKeysListResponse$Outbound, z.ZodTypeDef, LicenseKeysListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace LicenseKeysListResponse$ {
    /** @deprecated use `LicenseKeysListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<LicenseKeysListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `LicenseKeysListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<LicenseKeysListResponse$Outbound, z.ZodTypeDef, LicenseKeysListResponse>;
    /** @deprecated use `LicenseKeysListResponse$Outbound` instead. */
    type Outbound = LicenseKeysListResponse$Outbound;
}
export declare function licenseKeysListResponseToJSON(licenseKeysListResponse: LicenseKeysListResponse): string;
export declare function licenseKeysListResponseFromJSON(jsonString: string): SafeParseResult<LicenseKeysListResponse, SDKValidationError>;
//# sourceMappingURL=licensekeyslist.d.ts.map