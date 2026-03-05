import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { MetadataQuery, MetadataQuery$Outbound } from "../components/customerslist.js";
import { EventSortProperty } from "../components/eventsortproperty.js";
import { EventSource } from "../components/eventsource.js";
import { ListResourceEvent, ListResourceEvent$Outbound } from "../components/listresourceevent.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
/**
 * Filter by organization ID.
 */
export type EventsListQueryParamOrganizationIDFilter = string | Array<string>;
/**
 * Filter by customer ID.
 */
export type EventsListQueryParamCustomerIDFilter = string | Array<string>;
/**
 * Filter by external customer ID.
 */
export type ExternalCustomerIDFilter = string | Array<string>;
/**
 * Filter by event source.
 */
export type SourceFilter = EventSource | Array<EventSource>;
export type EventsListRequest = {
    /**
     * Filter events after this timestamp.
     */
    startTimestamp?: Date | null | undefined;
    /**
     * Filter events before this timestamp.
     */
    endTimestamp?: Date | null | undefined;
    /**
     * Filter by organization ID.
     */
    organizationId?: string | Array<string> | null | undefined;
    /**
     * Filter by customer ID.
     */
    customerId?: string | Array<string> | null | undefined;
    /**
     * Filter by external customer ID.
     */
    externalCustomerId?: string | Array<string> | null | undefined;
    /**
     * Filter by event source.
     */
    source?: EventSource | Array<EventSource> | null | undefined;
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
    sorting?: Array<EventSortProperty> | null | undefined;
    /**
     * Filter by metadata key-value pairs. It uses the `deepObject` style, e.g. `?metadata[key]=value`.
     */
    metadata?: {
        [k: string]: MetadataQuery;
    } | null | undefined;
};
export type EventsListResponse = {
    result: ListResourceEvent;
};
/** @internal */
export declare const EventsListQueryParamOrganizationIDFilter$inboundSchema: z.ZodType<EventsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsListQueryParamOrganizationIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const EventsListQueryParamOrganizationIDFilter$outboundSchema: z.ZodType<EventsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, EventsListQueryParamOrganizationIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsListQueryParamOrganizationIDFilter$ {
    /** @deprecated use `EventsListQueryParamOrganizationIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsListQueryParamOrganizationIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsListQueryParamOrganizationIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsListQueryParamOrganizationIDFilter$Outbound, z.ZodTypeDef, EventsListQueryParamOrganizationIDFilter>;
    /** @deprecated use `EventsListQueryParamOrganizationIDFilter$Outbound` instead. */
    type Outbound = EventsListQueryParamOrganizationIDFilter$Outbound;
}
export declare function eventsListQueryParamOrganizationIDFilterToJSON(eventsListQueryParamOrganizationIDFilter: EventsListQueryParamOrganizationIDFilter): string;
export declare function eventsListQueryParamOrganizationIDFilterFromJSON(jsonString: string): SafeParseResult<EventsListQueryParamOrganizationIDFilter, SDKValidationError>;
/** @internal */
export declare const EventsListQueryParamCustomerIDFilter$inboundSchema: z.ZodType<EventsListQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsListQueryParamCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const EventsListQueryParamCustomerIDFilter$outboundSchema: z.ZodType<EventsListQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, EventsListQueryParamCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsListQueryParamCustomerIDFilter$ {
    /** @deprecated use `EventsListQueryParamCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsListQueryParamCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsListQueryParamCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsListQueryParamCustomerIDFilter$Outbound, z.ZodTypeDef, EventsListQueryParamCustomerIDFilter>;
    /** @deprecated use `EventsListQueryParamCustomerIDFilter$Outbound` instead. */
    type Outbound = EventsListQueryParamCustomerIDFilter$Outbound;
}
export declare function eventsListQueryParamCustomerIDFilterToJSON(eventsListQueryParamCustomerIDFilter: EventsListQueryParamCustomerIDFilter): string;
export declare function eventsListQueryParamCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<EventsListQueryParamCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const ExternalCustomerIDFilter$inboundSchema: z.ZodType<ExternalCustomerIDFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type ExternalCustomerIDFilter$Outbound = string | Array<string>;
/** @internal */
export declare const ExternalCustomerIDFilter$outboundSchema: z.ZodType<ExternalCustomerIDFilter$Outbound, z.ZodTypeDef, ExternalCustomerIDFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace ExternalCustomerIDFilter$ {
    /** @deprecated use `ExternalCustomerIDFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<ExternalCustomerIDFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `ExternalCustomerIDFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<ExternalCustomerIDFilter$Outbound, z.ZodTypeDef, ExternalCustomerIDFilter>;
    /** @deprecated use `ExternalCustomerIDFilter$Outbound` instead. */
    type Outbound = ExternalCustomerIDFilter$Outbound;
}
export declare function externalCustomerIDFilterToJSON(externalCustomerIDFilter: ExternalCustomerIDFilter): string;
export declare function externalCustomerIDFilterFromJSON(jsonString: string): SafeParseResult<ExternalCustomerIDFilter, SDKValidationError>;
/** @internal */
export declare const SourceFilter$inboundSchema: z.ZodType<SourceFilter, z.ZodTypeDef, unknown>;
/** @internal */
export type SourceFilter$Outbound = string | Array<string>;
/** @internal */
export declare const SourceFilter$outboundSchema: z.ZodType<SourceFilter$Outbound, z.ZodTypeDef, SourceFilter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SourceFilter$ {
    /** @deprecated use `SourceFilter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SourceFilter, z.ZodTypeDef, unknown>;
    /** @deprecated use `SourceFilter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SourceFilter$Outbound, z.ZodTypeDef, SourceFilter>;
    /** @deprecated use `SourceFilter$Outbound` instead. */
    type Outbound = SourceFilter$Outbound;
}
export declare function sourceFilterToJSON(sourceFilter: SourceFilter): string;
export declare function sourceFilterFromJSON(jsonString: string): SafeParseResult<SourceFilter, SDKValidationError>;
/** @internal */
export declare const EventsListRequest$inboundSchema: z.ZodType<EventsListRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsListRequest$Outbound = {
    start_timestamp?: string | null | undefined;
    end_timestamp?: string | null | undefined;
    organization_id?: string | Array<string> | null | undefined;
    customer_id?: string | Array<string> | null | undefined;
    external_customer_id?: string | Array<string> | null | undefined;
    source?: string | Array<string> | null | undefined;
    page: number;
    limit: number;
    sorting?: Array<string> | null | undefined;
    metadata?: {
        [k: string]: MetadataQuery$Outbound;
    } | null | undefined;
};
/** @internal */
export declare const EventsListRequest$outboundSchema: z.ZodType<EventsListRequest$Outbound, z.ZodTypeDef, EventsListRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsListRequest$ {
    /** @deprecated use `EventsListRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsListRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsListRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsListRequest$Outbound, z.ZodTypeDef, EventsListRequest>;
    /** @deprecated use `EventsListRequest$Outbound` instead. */
    type Outbound = EventsListRequest$Outbound;
}
export declare function eventsListRequestToJSON(eventsListRequest: EventsListRequest): string;
export declare function eventsListRequestFromJSON(jsonString: string): SafeParseResult<EventsListRequest, SDKValidationError>;
/** @internal */
export declare const EventsListResponse$inboundSchema: z.ZodType<EventsListResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsListResponse$Outbound = {
    Result: ListResourceEvent$Outbound;
};
/** @internal */
export declare const EventsListResponse$outboundSchema: z.ZodType<EventsListResponse$Outbound, z.ZodTypeDef, EventsListResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsListResponse$ {
    /** @deprecated use `EventsListResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsListResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsListResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsListResponse$Outbound, z.ZodTypeDef, EventsListResponse>;
    /** @deprecated use `EventsListResponse$Outbound` instead. */
    type Outbound = EventsListResponse$Outbound;
}
export declare function eventsListResponseToJSON(eventsListResponse: EventsListResponse): string;
export declare function eventsListResponseFromJSON(jsonString: string): SafeParseResult<EventsListResponse, SDKValidationError>;
//# sourceMappingURL=eventslist.d.ts.map