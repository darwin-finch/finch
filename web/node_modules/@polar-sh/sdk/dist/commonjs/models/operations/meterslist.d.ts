import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { MetadataQuery, MetadataQuery$Outbound } from "../components/customerslist.js";
import { ListResourceMeter, ListResourceMeter$Outbound } from "../components/listresourcemeter.js";
import { MeterSortProperty } from "../components/metersortproperty.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type MetersListQueryParamOrganizationIDFilter = string | Array<string>;
export type MetersListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by name.
     */
    query?: string | null | undefined;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
    /**
     * Sorting criterion. Several criteria can be used simultaneously and will be applied in order. Add a minus sign `-` before the criteria name to sort by descending order.
     */
    sorting?: Array<MeterSortProperty> | null | undefined;
    /**
     * Filter by metadata key-value pairs. It uses the `deepObject` style, e.g. `?metadata[key]=value`.
     */
    metadata?: {
        [k: string]: MetadataQuery;
    } | null | undefined;
};
export type MetersListResponse = {
    result: ListResourceMeter;
};
/** @internal */
export declare const MetersListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<MetersListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const MetersListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<MetersListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, MetersListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `MetersListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, MetersListQueryParamOrganizationIDFilter>;
    /** @deprecated use `MetersListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = MetersListQueryParamOrganizationIDFilter$Outbound;
}
export declare function metersListQueryParamOrganizationIDFilterToJSON(metersListQueryParamOrganizationIDFilter: MetersListQueryParamOrganizationIDFilter): string;
export declare function metersListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<MetersListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const MetersListRequest$inboundSchema: z.ZodType<MetersListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    query?: string | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
    metadata?: {
        [k: string]: MetadataQuery$Outbound;
    } | null | undefined;
};
/** @internal */
export declare const MetersListRequest$outboundSchema: z.ZodType<MetersListRequest$Outbound, z.ZodTypeDef, MetersListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersListRequest$ {
    /** @deprecated use `MetersListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersListRequest$Outbound, z.ZodTypeDef, MetersListRequest>;
    /** @deprecated use `MetersListRequest$Outbound` instead. */
    type Outbound = MetersListRequest$Outbound;
}
export declare function metersListRequestToJSON(metersListRequest: MetersListRequest): string;
export declare function metersListRequestFromJSON(jsonString: string): SafeParseResult<MetersListRequest, SDKValidationError>;
/** @internal */
export declare const MetersListResponse$inboundSchema: z.ZodType<MetersListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersListResponse$Outbound = {
    Result: ListResourceMeter$Outbound;
};
/** @internal */
export declare const MetersListResponse$outboundSchema: z.ZodType<MetersListResponse$Outbound, z.ZodTypeDef, MetersListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersListResponse$ {
    /** @deprecated use `MetersListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersListResponse$Outbound, z.ZodTypeDef, MetersListResponse>;
    /** @deprecated use `MetersListResponse$Outbound` instead. */
    type Outbound = MetersListResponse$Outbound;
}
export declare function metersListResponseToJSON(metersListResponse: MetersListResponse): string;
export declare function metersListResponseFromJSON(jsonString: string): SafeParseResult<MetersListResponse, SDKValidationError>;
//# sourceMappingURL=meterslist.d.ts.map