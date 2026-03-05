import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Customer, Customer$Outbound } from "./customer.js";
import { EventSource } from "./eventsource.js";
export type EventMetadata = string | number | boolean;
export type Event = {
    metadata: {
        [k: string]: string | number | boolean;
    };
    /**
     * The ID of the object.
     */
    id: string;
    /**
     * The timestamp of the event.
     */
    timestamp: Date;
    /**
     * The name of the event.
     */
    name: string;
    source: EventSource;
    /**
     * The ID of the organization owning the event.
     */
    organizationId: string;
    /**
     * ID of the customer in your Polar organization associated with the event.
     */
    customerId: string | null;
    /**
     * The customer associated with the event.
     */
    customer: Customer | null;
    /**
     * ID of the customer in your system associated with the event.
     */
    externalCustomerId: string | null;
};
/** @internal */
export declare const EventMetadata$inboundSchema: z.ZodType<EventMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type EventMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const EventMetadata$outboundSchema: z.ZodType<EventMetadata$Outbound, z.ZodTypeDef, EventMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventMetadata$ {
    /** @deprecated use `EventMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventMetadata$Outbound, z.ZodTypeDef, EventMetadata>;
    /** @deprecated use `EventMetadata$Outbound` instead. */
    type Outbound = EventMetadata$Outbound;
}
export declare function eventMetadataToJSON(eventMetadata: EventMetadata): string;
export declare function eventMetadataFromJSON(jsonString: string): SafeParseResult<EventMetadata, SDKValidationError>;
/** @internal */
export declare const Event$inboundSchema: z.ZodType<Event, z.ZodTypeDef, unknown>;
/** @internal */
export type Event$Outbound = {
    metadata: {
        [k: string]: string | number | boolean;
    };
    id: string;
    timestamp: string;
    name: string;
    source: string;
    organization_id: string;
    customer_id: string | null;
    customer: Customer$Outbound | null;
    external_customer_id: string | null;
};
/** @internal */
export declare const Event$outboundSchema: z.ZodType<Event$Outbound, z.ZodTypeDef, Event>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Event$ {
    /** @deprecated use `Event$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Event, z.ZodTypeDef, unknown>;
    /** @deprecated use `Event$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Event$Outbound, z.ZodTypeDef, Event>;
    /** @deprecated use `Event$Outbound` instead. */
    type Outbound = Event$Outbound;
}
export declare function eventToJSON(event: Event): string;
export declare function eventFromJSON(jsonString: string): SafeParseResult<Event, SDKValidationError>;
//# sourceMappingURL=event.d.ts.map