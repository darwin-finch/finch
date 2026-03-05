import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CountAggregation, CountAggregation$Outbound } from "./countaggregation.js";
import { Filter, Filter$Outbound } from "./filter.js";
import { PropertyAggregation, PropertyAggregation$Outbound } from "./propertyaggregation.js";
export type MeterCreateMetadata = string | number | boolean;
/**
 * The aggregation to apply on the filtered events to calculate the meter.
 */
export type MeterCreateAggregation = (CountAggregation & {
    func: "count";
}) | (PropertyAggregation & {
    func: "avg";
}) | (PropertyAggregation & {
    func: "max";
}) | (PropertyAggregation & {
    func: "min";
}) | (PropertyAggregation & {
    func: "sum";
});
export type MeterCreate = {
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
     * The name of the meter. Will be shown on customer's invoices and usage.
     */
    name: string;
    filter: Filter;
    /**
     * The aggregation to apply on the filtered events to calculate the meter.
     */
    aggregation: (CountAggregation & {
        func: "count";
    }) | (PropertyAggregation & {
        func: "avg";
    }) | (PropertyAggregation & {
        func: "max";
    }) | (PropertyAggregation & {
        func: "min";
    }) | (PropertyAggregation & {
        func: "sum";
    });
    /**
     * The ID of the organization owning the meter. **Required unless you use an organization token.**
     */
    organizationId?: string | null | undefined;
};
/** @internal */
export declare const MeterCreateMetadata$inboundSchema: z.ZodType<MeterCreateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterCreateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const MeterCreateMetadata$outboundSchema: z.ZodType<MeterCreateMetadata$Outbound, z.ZodTypeDef, MeterCreateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterCreateMetadata$ {
    /** @deprecated use `MeterCreateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterCreateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterCreateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterCreateMetadata$Outbound, z.ZodTypeDef, MeterCreateMetadata>;
    /** @deprecated use `MeterCreateMetadata$Outbound` instead. */
    type Outbound = MeterCreateMetadata$Outbound;
}
export declare function meterCreateMetadataToJSON(meterCreateMetadata: MeterCreateMetadata): string;
export declare function meterCreateMetadataFromJSON(jsonString: string): SafeParseResult<MeterCreateMetadata, SDKValidationError>;
/** @internal */
export declare const MeterCreateAggregation$inboundSchema: z.ZodType<MeterCreateAggregation, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterCreateAggregation$Outbound = (CountAggregation$Outbound & {
    func: "count";
}) | (PropertyAggregation$Outbound & {
    func: "avg";
}) | (PropertyAggregation$Outbound & {
    func: "max";
}) | (PropertyAggregation$Outbound & {
    func: "min";
}) | (PropertyAggregation$Outbound & {
    func: "sum";
});
/** @internal */
export declare const MeterCreateAggregation$outboundSchema: z.ZodType<MeterCreateAggregation$Outbound, z.ZodTypeDef, MeterCreateAggregation>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterCreateAggregation$ {
    /** @deprecated use `MeterCreateAggregation$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterCreateAggregation, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterCreateAggregation$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterCreateAggregation$Outbound, z.ZodTypeDef, MeterCreateAggregation>;
    /** @deprecated use `MeterCreateAggregation$Outbound` instead. */
    type Outbound = MeterCreateAggregation$Outbound;
}
export declare function meterCreateAggregationToJSON(meterCreateAggregation: MeterCreateAggregation): string;
export declare function meterCreateAggregationFromJSON(jsonString: string): SafeParseResult<MeterCreateAggregation, SDKValidationError>;
/** @internal */
export declare const MeterCreate$inboundSchema: z.ZodType<MeterCreate, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterCreate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | undefined;
    name: string;
    filter: Filter$Outbound;
    aggregation: (CountAggregation$Outbound & {
        func: "count";
    }) | (PropertyAggregation$Outbound & {
        func: "avg";
    }) | (PropertyAggregation$Outbound & {
        func: "max";
    }) | (PropertyAggregation$Outbound & {
        func: "min";
    }) | (PropertyAggregation$Outbound & {
        func: "sum";
    });
    organization_id?: string | null | undefined;
};
/** @internal */
export declare const MeterCreate$outboundSchema: z.ZodType<MeterCreate$Outbound, z.ZodTypeDef, MeterCreate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterCreate$ {
    /** @deprecated use `MeterCreate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterCreate, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterCreate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterCreate$Outbound, z.ZodTypeDef, MeterCreate>;
    /** @deprecated use `MeterCreate$Outbound` instead. */
    type Outbound = MeterCreate$Outbound;
}
export declare function meterCreateToJSON(meterCreate: MeterCreate): string;
export declare function meterCreateFromJSON(jsonString: string): SafeParseResult<MeterCreate, SDKValidationError>;
//# sourceMappingURL=metercreate.d.ts.map