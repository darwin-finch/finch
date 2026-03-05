import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CountAggregation, CountAggregation$Outbound } from "./countaggregation.js";
import { Filter, Filter$Outbound } from "./filter.js";
import { PropertyAggregation, PropertyAggregation$Outbound } from "./propertyaggregation.js";
export type MeterUpdateMetadata = string | number | boolean;
export type Aggregation = (CountAggregation & {
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
export type MeterUpdate = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    /**
     * The name of the meter. Will be shown on customer's invoices and usage.
     */
    name?: string | null | undefined;
    /**
     * The filter to apply on events that'll be used to calculate the meter.
     */
    filter?: Filter | null | undefined;
    /**
     * The aggregation to apply on the filtered events to calculate the meter.
     */
    aggregation?: (CountAggregation & {
        func: "count";
    }) | (PropertyAggregation & {
        func: "avg";
    }) | (PropertyAggregation & {
        func: "max";
    }) | (PropertyAggregation & {
        func: "min";
    }) | (PropertyAggregation & {
        func: "sum";
    }) | null | undefined;
};
/** @internal */
export declare const MeterUpdateMetadata$inboundSchema: z.ZodType<MeterUpdateMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterUpdateMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const MeterUpdateMetadata$outboundSchema: z.ZodType<MeterUpdateMetadata$Outbound, z.ZodTypeDef, MeterUpdateMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterUpdateMetadata$ {
    /** @deprecated use `MeterUpdateMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterUpdateMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterUpdateMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterUpdateMetadata$Outbound, z.ZodTypeDef, MeterUpdateMetadata>;
    /** @deprecated use `MeterUpdateMetadata$Outbound` instead. */
    type Outbound = MeterUpdateMetadata$Outbound;
}
export declare function meterUpdateMetadataToJSON(meterUpdateMetadata: MeterUpdateMetadata): string;
export declare function meterUpdateMetadataFromJSON(jsonString: string): SafeParseResult<MeterUpdateMetadata, SDKValidationError>;
/** @internal */
export declare const Aggregation$inboundSchema: z.ZodType<Aggregation, z.ZodTypeDef, unknown>;
/** @internal */
export type Aggregation$Outbound = (CountAggregation$Outbound & {
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
export declare const Aggregation$outboundSchema: z.ZodType<Aggregation$Outbound, z.ZodTypeDef, Aggregation>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Aggregation$ {
    /** @deprecated use `Aggregation$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Aggregation, z.ZodTypeDef, unknown>;
    /** @deprecated use `Aggregation$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Aggregation$Outbound, z.ZodTypeDef, Aggregation>;
    /** @deprecated use `Aggregation$Outbound` instead. */
    type Outbound = Aggregation$Outbound;
}
export declare function aggregationToJSON(aggregation: Aggregation): string;
export declare function aggregationFromJSON(jsonString: string): SafeParseResult<Aggregation, SDKValidationError>;
/** @internal */
export declare const MeterUpdate$inboundSchema: z.ZodType<MeterUpdate, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterUpdate$Outbound = {
    metadata?: {
        [k: string]: string | number | boolean;
    } | null | undefined;
    name?: string | null | undefined;
    filter?: Filter$Outbound | null | undefined;
    aggregation?: (CountAggregation$Outbound & {
        func: "count";
    }) | (PropertyAggregation$Outbound & {
        func: "avg";
    }) | (PropertyAggregation$Outbound & {
        func: "max";
    }) | (PropertyAggregation$Outbound & {
        func: "min";
    }) | (PropertyAggregation$Outbound & {
        func: "sum";
    }) | null | undefined;
};
/** @internal */
export declare const MeterUpdate$outboundSchema: z.ZodType<MeterUpdate$Outbound, z.ZodTypeDef, MeterUpdate>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterUpdate$ {
    /** @deprecated use `MeterUpdate$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterUpdate, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterUpdate$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterUpdate$Outbound, z.ZodTypeDef, MeterUpdate>;
    /** @deprecated use `MeterUpdate$Outbound` instead. */
    type Outbound = MeterUpdate$Outbound;
}
export declare function meterUpdateToJSON(meterUpdate: MeterUpdate): string;
export declare function meterUpdateFromJSON(jsonString: string): SafeParseResult<MeterUpdate, SDKValidationError>;
//# sourceMappingURL=meterupdate.d.ts.map