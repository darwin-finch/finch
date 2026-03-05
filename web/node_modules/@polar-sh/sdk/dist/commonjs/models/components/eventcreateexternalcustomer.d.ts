import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type EventCreateExternalCustomerMetadata = string | number | boolean;
export type EventCreateExternalCustomer = {
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
     * ID of the customer in your system associated with the event.
     */
    externalCustomerId: string;
};
/** @internal */
export declare const EventCreateExternalCustomerMetadata$inboundSchema: z.ZodType<EventCreateExternalCustomerMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type EventCreateExternalCustomerMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const EventCreateExternalCustomerMetadata$outboundSchema: z.ZodType<EventCreateExternalCustomerMetadata$Outbound, z.ZodTypeDef, EventCreateExternalCustomerMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventCreateExternalCustomerMetadata$ {
    /** @deprecated use `EventCreateExternalCustomerMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventCreateExternalCustomerMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventCreateExternalCustomerMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventCreateExternalCustomerMetadata$Outbound, z.ZodTypeDef, EventCreateExternalCustomerMetadata>;
    /** @deprecated use `EventCreateExternalCustomerMetadata$Outbound` instead. */
    type Outbound = EventCreateExternalCustomerMetadata$Outbound;
}
export declare function eventCreateExternalCustomerMetadataToJSON(eventCreateExternalCustomerMetadata: EventCreateExternalCustomerMetadata): string;
export declare function eventCreateExternalCustomerMetadataFromJSON(jsonString: string): SafeParseResult<EventCreateExternalCustomerMetadata, SDKValidationError>;
/** @internal */
export declare const EventCreateExternalCustomer$inboundSchema: z.ZodType<EventCreateExternalCustomer, z.ZodTypeDef, unknown>;
/** @internal */
export type EventCreateExternalCustomer$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    timestamp?: string | undefined;
    name: string;
    organization_id?: string | null | undefined;
    external_customer_id: string;
};
/** @internal */
export declare const EventCreateExternalCustomer$outboundSchema: z.ZodType<EventCreateExternalCustomer$Outbound, z.ZodTypeDef, EventCreateExternalCustomer>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace EventCreateExternalCustomer$ {
    /** @deprecated use `EventCreateExternalCustomer$inboundSchema` instead. */
    const inboundSchema: z.ZodType<EventCreateExternalCustomer, z.ZodTypeDef, unknown>;
    /** @deprecated use `EventCreateExternalCustomer$outboundSchema` instead. */
    const outboundSchema: z.ZodType<EventCreateExternalCustomer$Outbound, z.ZodTypeDef, EventCreateExternalCustomer>;
    /** @deprecated use `EventCreateExternalCustomer$Outbound` instead. */
    type Outbound = EventCreateExternalCustomer$Outbound;
}
export declare function eventCreateExternalCustomerToJSON(eventCreateExternalCustomer: EventCreateExternalCustomer): string;
export declare function eventCreateExternalCustomerFromJSON(jsonString: string): SafeParseResult<EventCreateExternalCustomer, SDKValidationError>;
//# sourceMappingURL=eventcreateexternalcustomer.d.ts.map