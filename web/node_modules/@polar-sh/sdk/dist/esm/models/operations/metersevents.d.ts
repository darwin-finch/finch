import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { ListResourceEvent, ListResourceEvent$Outbound } from "../components/listresourceevent.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type MetersEventsRequest = {
    /**
     * The meter ID.
     */
    id: string;
    /**
     * Page number, defaults to 1.
     */
    page?: number | undefined;
    /**
     * Size of a page, defaults to 10. Maximum is 100.
     */
    limit?: number | undefined;
};
export type MetersEventsResponse = {
    result: ListResourceEvent;
};
/** @internal */
export declare const MetersEventsRequest$inboundSchema: z.ZodType<MetersEventsRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersEventsRequest$Outbound = {
    id: string;
    page: number;
    limit: number;
};
/** @internal */
export declare const MetersEventsRequest$outboundSchema: z.ZodType<MetersEventsRequest$Outbound, z.ZodTypeDef, MetersEventsRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersEventsRequest$ {
    /** @deprecated use `MetersEventsRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersEventsRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersEventsRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersEventsRequest$Outbound, z.ZodTypeDef, MetersEventsRequest>;
    /** @deprecated use `MetersEventsRequest$Outbound` instead. */
    type Outbound = MetersEventsRequest$Outbound;
}
export declare function metersEventsRequestToJSON(metersEventsRequest: MetersEventsRequest): string;
export declare function metersEventsRequestFromJSON(jsonString: string): SafeParseResult<MetersEventsRequest, SDKValidationError>;
/** @internal */
export declare const MetersEventsResponse$inboundSchema: z.ZodType<MetersEventsResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type MetersEventsResponse$Outbound = {
    Result: ListResourceEvent$Outbound;
};
/** @internal */
export declare const MetersEventsResponse$outboundSchema: z.ZodType<MetersEventsResponse$Outbound, z.ZodTypeDef, MetersEventsResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MetersEventsResponse$ {
    /** @deprecated use `MetersEventsResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MetersEventsResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `MetersEventsResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MetersEventsResponse$Outbound, z.ZodTypeDef, MetersEventsResponse>;
    /** @deprecated use `MetersEventsResponse$Outbound` instead. */
    type Outbound = MetersEventsResponse$Outbound;
}
export declare function metersEventsResponseToJSON(metersEventsResponse: MetersEventsResponse): string;
export declare function metersEventsResponseFromJSON(jsonString: string): SafeParseResult<MetersEventsResponse, SDKValidationError>;
//# sourceMappingURL=metersevents.d.ts.map