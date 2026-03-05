import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceBenefitGrant, ListResourceBenefitGrant$Outbound } from "../components/listresourcebenefitgrant.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by customer.
 */
export type QueryParamCustomerIDFilter = string | Array<string>;
export type BenefitsGrantsRequest = {
    id: string;
    /**
     * Filter by granted status. If `true`, only granted benefits will be returned. If `false`, only revoked benefits will be returned.
     */
    isGranted?: boolean | null | undefined;
    /**
     * Filter by customer.
     */
    customerId?: string | Array<string> | null | undefined;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
};
export type BenefitsGrantsResponse = {
    result: ListResourceBenefitGrant;
};
/** @internal */
export declare const QueryParamCustomerIDFilter$inboundSchema: z.ZodType<QueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamCustomerIDFilter$outboundSchema: z.ZodType<QueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, QueryParamCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamCustomerIDFilter$ {
    /** @deprecated use `QueryParamCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, QueryParamCustomerIDFilter>;
    /** @deprecated use `QueryParamCustomerIDFilter$Outbound` instead. */
    type Outbound = QueryParamCustomerIDFilter$Outbound;
}
export declare function queryParamCustomerIDFilterToJSON(queryParamCustomerIDFilter: QueryParamCustomerIDFilter): string;
export declare function queryParamCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const BenefitsGrantsRequest$inboundSchema: z.ZodType<BenefitsGrantsRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitsGrantsRequest$Outbound = {
    id: string;
    is_granted?: boolean | null | undefined;
    customer_id?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
};
/** @internal */
export declare const BenefitsGrantsRequest$outboundSchema: z.ZodType<BenefitsGrantsRequest$Outbound, z.ZodTypeDef, BenefitsGrantsRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitsGrantsRequest$ {
    /** @deprecated use `BenefitsGrantsRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitsGrantsRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitsGrantsRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitsGrantsRequest$Outbound, z.ZodTypeDef, BenefitsGrantsRequest>;
    /** @deprecated use `BenefitsGrantsRequest$Outbound` instead. */
    type Outbound = BenefitsGrantsRequest$Outbound;
}
export declare function benefitsGrantsRequestToJSON(benefitsGrantsRequest: BenefitsGrantsRequest): string;
export declare function benefitsGrantsRequestFromJSON(jsonString: string): SafeParseResult<BenefitsGrantsRequest, SDKValidationError>;
/** @internal */
export declare const BenefitsGrantsResponse$inboundSchema: z.ZodType<BenefitsGrantsResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitsGrantsResponse$Outbound = {
    Result: ListResourceBenefitGrant$Outbound;
};
/** @internal */
export declare const BenefitsGrantsResponse$outboundSchema: z.ZodType<BenefitsGrantsResponse$Outbound, z.ZodTypeDef, BenefitsGrantsResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitsGrantsResponse$ {
    /** @deprecated use `BenefitsGrantsResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitsGrantsResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitsGrantsResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitsGrantsResponse$Outbound, z.ZodTypeDef, BenefitsGrantsResponse>;
    /** @deprecated use `BenefitsGrantsResponse$Outbound` instead. */
    type Outbound = BenefitsGrantsResponse$Outbound;
}
export declare function benefitsGrantsResponseToJSON(benefitsGrantsResponse: BenefitsGrantsResponse): string;
export declare function benefitsGrantsResponseFromJSON(jsonString: string): SafeParseResult<BenefitsGrantsResponse, SDKValidationError>;
//# sourceMappingURL=benefitsgrants.d.ts.map