import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { TimeInterval } from "../components/timeinterval.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by customer ID.
 */
export type MetersQuantitiesQueryParamCustomerIDFilter = string | Array<string>;
/**
 * Filter by external customer ID.
 */
export type QueryParamExternalCustomerIDFilter = string | Array<string>;
export type MetersQuantitiesRequest = {
    /**
     * The meter ID.
     */
    id: string;
    /**
     * Start timestamp.
     */
    startTimestamp: Date;
    /**
     * End timestamp.
     */
    endTimestamp: Date;
    /**
     * Interval between two timestamps.
     */
    interval: TimeInterval;
    /**
     * Filter by customer ID.
     */
    customerId?: string | Array<string> | null | undefined;
    /**
     * Filter by external customer ID.
     */
    externalCustomerId?: string | Array<string> | null | undefined;
};
/** @internal */
export declare const MetersQuantitiesQueryParamCustomerIDFilter$inboundSchema: z.ZodType<MetersQuantitiesQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersQuantitiesQueryParamCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const MetersQuantitiesQueryParamCustomerIDFilter$outboundSchema: z.ZodType<MetersQuantitiesQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, MetersQuantitiesQueryParamCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersQuantitiesQueryParamCustomerIDFilter$ {
    /** @deprecated use `MetersQuantitiesQueryParamCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersQuantitiesQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersQuantitiesQueryParamCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersQuantitiesQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, MetersQuantitiesQueryParamCustomerIDFilter>;
    /** @deprecated use `MetersQuantitiesQueryParamCustomerIDFilter$Outbound` instead. */
    type Outbound = MetersQuantitiesQueryParamCustomerIDFilter$Outbound;
}
export declare function metersQuantitiesQueryParamCustomerIDFilterToJSON(metersQuantitiesQueryParamCustomerIDFilter: MetersQuantitiesQueryParamCustomerIDFilter): string;
export declare function metersQuantitiesQueryParamCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<MetersQuantitiesQueryParamCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const QueryParamExternalCustomerIDFilter$inboundSchema: z.ZodType<QueryParamExternalCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type QueryParamExternalCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const QueryParamExternalCustomerIDFilter$outboundSchema: z.ZodType<QueryParamExternalCustomerIDFilter$Outbound, z.ZodTypeDef, QueryParamExternalCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace QueryParamExternalCustomerIDFilter$ {
    /** @deprecated use `QueryParamExternalCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<QueryParamExternalCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `QueryParamExternalCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<QueryParamExternalCustomerIDFilter$Outbound, z.ZodTypeDef, QueryParamExternalCustomerIDFilter>;
    /** @deprecated use `QueryParamExternalCustomerIDFilter$Outbound` instead. */
    type Outbound = QueryParamExternalCustomerIDFilter$Outbound;
}
export declare function queryParamExternalCustomerIDFilterToJSON(queryParamExternalCustomerIDFilter: QueryParamExternalCustomerIDFilter): string;
export declare function queryParamExternalCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<QueryParamExternalCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const MetersQuantitiesRequest$inboundSchema: z.ZodType<MetersQuantitiesRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersQuantitiesRequest$Outbound = {
    id: string;
    start_timestamp: string;
    end_timestamp: string;
    interval: string;
    customer_id?: string | Array<string> | null | undefined;
    external_customer_id?: string | Array<string> | null | undefined;
};
/** @internal */
export declare const MetersQuantitiesRequest$outboundSchema: z.ZodType<MetersQuantitiesRequest$Outbound, z.ZodTypeDef, MetersQuantitiesRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersQuantitiesRequest$ {
    /** @deprecated use `MetersQuantitiesRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersQuantitiesRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersQuantitiesRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersQuantitiesRequest$Outbound, z.ZodTypeDef, MetersQuantitiesRequest>;
    /** @deprecated use `MetersQuantitiesRequest$Outbound` instead. */
    type Outbound = MetersQuantitiesRequest$Outbound;
}
export declare function metersQuantitiesRequestToJSON(metersQuantitiesRequest: MetersQuantitiesRequest): string;
export declare function metersQuantitiesRequestFromJSON(jsonString: string): SafeParseResult<MetersQuantitiesRequest, SDKValidationError>;
//# sourceMappingURL=metersquantities.d.ts.map