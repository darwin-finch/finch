import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { MetadataQuery, MetadataQuery$Outbound } from "../components/customerslist.js";
import { CustomerSortProperty } from "../components/customersortproperty.js";
import { ListResourceCustomer, ListResourceCustomer$Outbound } from "../components/listresourcecustomer.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type CustomersListQueryParamOrganizationIDFilter = string | Array<string>;
export type CustomersListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by exact email.
     */
    email?: string | null | undefined;
    /**
     * Filter by name or email.
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
    sorting?: Array<CustomerSortProperty> | null | undefined;
    /**
     * Filter by metadata key-value pairs. It uses the `deepObject` style, e.g. `?metadata[key]=value`.
     */
    metadata?: {
        [k: string]: MetadataQuery;
    } | null | undefined;
};
export type CustomersListResponse = {
    result: ListResourceCustomer;
};
/** @internal */
export declare const CustomersListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CustomersListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CustomersListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CustomersListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomersListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CustomersListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CustomersListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CustomersListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CustomersListQueryParamOrganizationIDFilter$Outbound;
}
export declare function customersListQueryParamOrganizationIDFilterToJSON(customersListQueryParamOrganizationIDFilter: CustomersListQueryParamOrganizationIDFilter): string;
export declare function customersListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CustomersListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const CustomersListRequest$inboundSchema: z.ZodType<CustomersListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    email?: string | null | undefined;
    query?: string | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
    metadata?: {
        [k: string]: MetadataQuery$Outbound;
    } | null | undefined;
};
/** @internal */
export declare const CustomersListRequest$outboundSchema: z.ZodType<CustomersListRequest$Outbound, z.ZodTypeDef, CustomersListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersListRequest$ {
    /** @deprecated use `CustomersListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersListRequest$Outbound, z.ZodTypeDef, CustomersListRequest>;
    /** @deprecated use `CustomersListRequest$Outbound` instead. */
    type Outbound = CustomersListRequest$Outbound;
}
export declare function customersListRequestToJSON(customersListRequest: CustomersListRequest): string;
export declare function customersListRequestFromJSON(jsonString: string): SafeParseResult<CustomersListRequest, SDKValidationError>;
/** @internal */
export declare const CustomersListResponse$inboundSchema: z.ZodType<CustomersListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CustomersListResponse$Outbound = {
    Result: ListResourceCustomer$Outbound;
};
/** @internal */
export declare const CustomersListResponse$outboundSchema: z.ZodType<CustomersListResponse$Outbound, z.ZodTypeDef, CustomersListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CustomersListResponse$ {
    /** @deprecated use `CustomersListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CustomersListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CustomersListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CustomersListResponse$Outbound, z.ZodTypeDef, CustomersListResponse>;
    /** @deprecated use `CustomersListResponse$Outbound` instead. */
    type Outbound = CustomersListResponse$Outbound;
}
export declare function customersListResponseToJSON(customersListResponse: CustomersListResponse): string;
export declare function customersListResponseFromJSON(jsonString: string): SafeParseResult<CustomersListResponse, SDKValidationError>;
//# sourceMappingURL=customerslist.d.ts.map