import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { CheckoutSortProperty } from "../components/checkoutsortproperty.js";
import { ListResourceCheckout, ListResourceCheckout$Outbound } from "../components/listresourcecheckout.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type CheckoutsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by product ID.
 */
export type CheckoutsListQueryParamProductIDFilter = string | Array<string>;
export type CheckoutsListRequest = {
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by product ID.
     */
    productId?: string | Array<string> | null | undefined;
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
    sorting?: Array<CheckoutSortProperty> | null | undefined;
};
export type CheckoutsListResponse = {
    result: ListResourceCheckout;
};
/** @internal */
export declare const CheckoutsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<CheckoutsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CheckoutsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<CheckoutsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CheckoutsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `CheckoutsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, CheckoutsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `CheckoutsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = CheckoutsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function checkoutsListQueryParamOrganizationIDFilterToJSON(checkoutsListQueryParamOrganizationIDFilter: CheckoutsListQueryParamOrganizationIDFilter): string;
export declare function checkoutsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<CheckoutsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const CheckoutsListQueryParamProductIDFilter$inboundSchema: z.ZodType<CheckoutsListQueryParamProductIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsListQueryParamProductIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const CheckoutsListQueryParamProductIDFilter$outboundSchema: z.ZodType<CheckoutsListQueryParamProductIDFilter$Outbound, z.ZodTypeDef, CheckoutsListQueryParamProductIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsListQueryParamProductIDFilter$ {
    /** @deprecated use `CheckoutsListQueryParamProductIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsListQueryParamProductIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsListQueryParamProductIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsListQueryParamProductIDFilter$Outbound, z.ZodTypeDef, CheckoutsListQueryParamProductIDFilter>;
    /** @deprecated use `CheckoutsListQueryParamProductIDFilter$Outbound` instead. */
    type Outbound = CheckoutsListQueryParamProductIDFilter$Outbound;
}
export declare function checkoutsListQueryParamProductIDFilterToJSON(checkoutsListQueryParamProductIDFilter: CheckoutsListQueryParamProductIDFilter): string;
export declare function checkoutsListQueryParamProductIDFilterFromJSON(jsonString: string): SafeParseResult<CheckoutsListQueryParamProductIDFilter, SDKValidationError>;
/** @internal */
export declare const CheckoutsListRequest$inboundSchema: z.ZodType<CheckoutsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsListRequest$Outbound = {
    organization_id?: string | Array<string> | null | undefined;
    product_id?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
};
/** @internal */
export declare const CheckoutsListRequest$outboundSchema: z.ZodType<CheckoutsListRequest$Outbound, z.ZodTypeDef, CheckoutsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsListRequest$ {
    /** @deprecated use `CheckoutsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsListRequest$Outbound, z.ZodTypeDef, CheckoutsListRequest>;
    /** @deprecated use `CheckoutsListRequest$Outbound` instead. */
    type Outbound = CheckoutsListRequest$Outbound;
}
export declare function checkoutsListRequestToJSON(checkoutsListRequest: CheckoutsListRequest): string;
export declare function checkoutsListRequestFromJSON(jsonString: string): SafeParseResult<CheckoutsListRequest, SDKValidationError>;
/** @internal */
export declare const CheckoutsListResponse$inboundSchema: z.ZodType<CheckoutsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type CheckoutsListResponse$Outbound = {
    Result: ListResourceCheckout$Outbound;
};
/** @internal */
export declare const CheckoutsListResponse$outboundSchema: z.ZodType<CheckoutsListResponse$Outbound, z.ZodTypeDef, CheckoutsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CheckoutsListResponse$ {
    /** @deprecated use `CheckoutsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CheckoutsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `CheckoutsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CheckoutsListResponse$Outbound, z.ZodTypeDef, CheckoutsListResponse>;
    /** @deprecated use `CheckoutsListResponse$Outbound` instead. */
    type Outbound = CheckoutsListResponse$Outbound;
}
export declare function checkoutsListResponseToJSON(checkoutsListResponse: CheckoutsListResponse): string;
export declare function checkoutsListResponseFromJSON(jsonString: string): SafeParseResult<CheckoutsListResponse, SDKValidationError>;
//# sourceMappingURL=checkoutslist.d.ts.map