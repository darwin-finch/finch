import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { EventCreateCustomer, EventCreateCustomer$Outbound } from "./eventcreatecustomer.js";
import { EventCreateExternalCustomer, EventCreateExternalCustomer$Outbound } from "./eventcreateexternalcustomer.js";
export type Events = EventCreateCustomer | EventCreateExternalCustomer;
export type EventsIngest = {
    /**
     * List of events to ingest.
     */
    events: Array<EventCreateCustomer | EventCreateExternalCustomer>;
};
/** @internal */
export declare const Events$inboundSchema: z.ZodType<Events, z.ZodTypeDef, unknown>;
/** @internal */
export type Events$Outbound = EventCreateCustomer$Outbound | EventCreateExternalCustomer$Outbound;
/** @internal */
export declare const Events$outboundSchema: z.ZodType<Events$Outbound, z.ZodTypeDef, Events>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Events$ {
    /** @deprecated use `Events$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Events, z.ZodTypeDef, unknown>;
    /** @deprecated use `Events$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Events$Outbound, z.ZodTypeDef, Events>;
    /** @deprecated use `Events$Outbound` instead. */
    type Outbound = Events$Outbound;
}
export declare function eventsToJSON(events: Events): string;
export declare function eventsFromJSON(jsonString: string): SafeParseResult<Events, SDKValidationError>;
/** @internal */
export declare const EventsIngest$inboundSchema: z.ZodType<EventsIngest, z.ZodTypeDef, unknown>;
/** @internal */
export type EventsIngest$Outbound = {
    events: Array<EventCreateCustomer$Outbound | EventCreateExternalCustomer$Outbound>;
};
/** @internal */
export declare const EventsIngest$outboundSchema: z.ZodType<EventsIngest$Outbound, z.ZodTypeDef, EventsIngest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventsIngest$ {
    /** @deprecated use `EventsIngest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventsIngest, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventsIngest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventsIngest$Outbound, z.ZodTypeDef, EventsIngest>;
    /** @deprecated use `EventsIngest$Outbound` instead. */
    type Outbound = EventsIngest$Outbound;
}
export declare function eventsIngestToJSON(eventsIngest: EventsIngest): string;
export declare function eventsIngestFromJSON(jsonString: string): SafeParseResult<EventsIngest, SDKValidationError>;
//# sourceMappingURL=eventsingest.d.ts.map