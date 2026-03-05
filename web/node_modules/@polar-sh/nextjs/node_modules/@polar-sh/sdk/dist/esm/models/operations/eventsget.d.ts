import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type EventsGetRequest = {
    /**
     * The event ID.
     */
    id: string;
};
/** @internal */
export declare const EventsGetRequest$inboundSchema: z.ZodType<EventsGetRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsGetRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const EventsGetRequest$outboundSchema: z.ZodType<EventsGetRequest$Outbound, z.ZodTypeDef, EventsGetRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsGetRequest$ {
    /** @deprecated use `EventsGetRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsGetRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsGetRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsGetRequest$Outbound, z.ZodTypeDef, EventsGetRequest>;
    /** @deprecated use `EventsGetRequest$Outbound` instead. */
    type Outbound = EventsGetRequest$Outbound;
}
export declare function eventsGetRequestToJSON(eventsGetRequest: EventsGetRequest): string;
export declare function eventsGetRequestFromJSON(jsonString: string): SafeParseResult<EventsGetRequest, SDKValidationError>;
//# sourceMappingURL=eventsget.d.ts.map