import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type EventsIngestResponse = {
    /**
     * Number of events inserted.
     */
    inserted: number;
};
/** @internal */
export declare const EventsIngestResponse$inboundSchema: z.ZodType<EventsIngestResponse, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsIngestResponse$Outbound = {
    inserted: number;
};
/** @internal */
export declare const EventsIngestResponse$outboundSchema: z.ZodType<EventsIngestResponse$Outbound, z.ZodTypeDef, EventsIngestResponse>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsIngestResponse$ {
    /** @deprecated use `EventsIngestResponse$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsIngestResponse, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsIngestResponse$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsIngestResponse$Outbound, z.ZodTypeDef, EventsIngestResponse>;
    /** @deprecated use `EventsIngestResponse$Outbound` instead. */
    type Outbound = EventsIngestResponse$Outbound;
}
export declare function eventsIngestResponseToJSON(eventsIngestResponse: EventsIngestResponse): string;
export declare function eventsIngestResponseFromJSON(jsonString: string): SafeParseResult<EventsIngestResponse, SDKValidationError>;
//# sourceMappingURL=eventsingestresponse.d.ts.map