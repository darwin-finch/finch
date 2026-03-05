import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type EventCreateCustomerMetadata = string | number | boolean;
export type EventCreateCustomer = {
    /**
     * Key-value object allowing you to store additional information.
     *
     * @remarks
     *
     * The key must be a string with a maximum length of **40 characters**.
     * The value must be either:
     *
     * * A string with a maximum length of **500 characters**
     * * An integer
     * * A boolean
     *
     * You can store up to **50 key-value pairs**.
     */
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    /**
     * The timestamp of the event.
     */
    timestamp?: Date | undefined;
    /**
     * The name of the event.
     */
    name: string;
    /**
     * The ID of the organization owning the event. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
    /**
     * ID of the customer in your Polar organization associated with the event.
     */
    customerId: string;
};
/** @internal */
export declare const EventCreateCustomerMetadata$inboundSchema: z.ZodType<EventCreateCustomerMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type EventCreateCustomerMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const EventCreateCustomerMetadata$outboundSchema: z.ZodType<EventCreateCustomerMetadata$Outbound, z.ZodTypeDef, EventCreateCustomerMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventCreateCustomerMetadata$ {
    /** @deprecated use `EventCreateCustomerMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventCreateCustomerMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventCreateCustomerMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventCreateCustomerMetadata$Outbound, z.ZodTypeDef, EventCreateCustomerMetadata>;
    /** @deprecated use `EventCreateCustomerMetadata$Outbound` instead. */
    type Outbound = EventCreateCustomerMetadata$Outbound;
}
export declare function eventCreateCustomerMetadataToJSON(eventCreateCustomerMetadata: EventCreateCustomerMetadata): string;
export declare function eventCreateCustomerMetadataFromJSON(jsonString: string): SafeParseResult<EventCreateCustomerMetadata, SDKValidationError>;
/** @internal */
export declare const EventCreateCustomer$inboundSchema: z.ZodType<EventCreateCustomer, z.ZodTypeDef, unknown>;
/** @internal */
export type EventCreateCustomer$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    timestamp?: string | undefined;
    name: string;
    organization_id?: string | null | undefined;
    customer_id: string;
};
/** @internal */
export declare const EventCreateCustomer$outboundSchema: z.ZodType<EventCreateCustomer$Outbound, z.ZodTypeDef, EventCreateCustomer>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventCreateCustomer$ {
    /** @deprecated use `EventCreateCustomer$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventCreateCustomer, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventCreateCustomer$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventCreateCustomer$Outbound, z.ZodTypeDef, EventCreateCustomer>;
    /** @deprecated use `EventCreateCustomer$Outbound` instead. */
    type Outbound = EventCreateCustomer$Outbound;
}
export declare function eventCreateCustomerToJSON(eventCreateCustomer: EventCreateCustomer): string;
export declare function eventCreateCustomerFromJSON(jsonString: string): SafeParseResult<EventCreateCustomer, SDKValidationError>;
//# sourceMappingURL=eventcreatecustomer.d.ts.map