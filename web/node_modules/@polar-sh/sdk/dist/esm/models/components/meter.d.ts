import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { CountAggregation, CountAggregation$Outbound } from "./countaggregation.js";
import { Filter, Filter$Outbound } from "./filter.js";
import { PropertyAggregation, PropertyAggregation$Outbound } from "./propertyaggregation.js";
export type MeterMetadata = string | number | boolean;
/**
 * The aggregation to apply on the filtered events to calculate the meter.
 */
export type MeterAggregation = (CountAggregation & {
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
export type Meter = {
    metadata: {
        [k: string]: string | number | boolean;
    };
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the object.
     */
    id: string;
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
     * The ID of the organization owning the meter.
     */
    organizationId: string;
};
/** @internal */
export declare const MeterMetadata$inboundSchema: z.ZodType<MeterMetadata, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterMetadata$Outbound = string | number | boolean;
/** @internal */
export declare const MeterMetadata$outboundSchema: z.ZodType<MeterMetadata$Outbound, z.ZodTypeDef, MeterMetadata>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterMetadata$ {
    /** @deprecated use `MeterMetadata$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterMetadata, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterMetadata$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterMetadata$Outbound, z.ZodTypeDef, MeterMetadata>;
    /** @deprecated use `MeterMetadata$Outbound` instead. */
    type Outbound = MeterMetadata$Outbound;
}
export declare function meterMetadataToJSON(meterMetadata: MeterMetadata): string;
export declare function meterMetadataFromJSON(jsonString: string): SafeParseResult<MeterMetadata, SDKValidationError>;
/** @internal */
export declare const MeterAggregation$inboundSchema: z.ZodType<MeterAggregation, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterAggregation$Outbound = (CountAggregation$Outbound & {
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
export declare const MeterAggregation$outboundSchema: z.ZodType<MeterAggregation$Outbound, z.ZodTypeDef, MeterAggregation>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterAggregation$ {
    /** @deprecated use `MeterAggregation$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterAggregation, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterAggregation$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterAggregation$Outbound, z.ZodTypeDef, MeterAggregation>;
    /** @deprecated use `MeterAggregation$Outbound` instead. */
    type Outbound = MeterAggregation$Outbound;
}
export declare function meterAggregationToJSON(meterAggregation: MeterAggregation): string;
export declare function meterAggregationFromJSON(jsonString: string): SafeParseResult<MeterAggregation, SDKValidationError>;
/** @internal */
export declare const Meter$inboundSchema: z.ZodType<Meter, z.ZodTypeDef, unknown>;
/** @internal */
export type Meter$Outbound = {
    metadata: {
        [k: string]: string | number | boolean;
    };
    created_at: string;
    modified_at: string | null;
    id: string;
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
    organization_id: string;
};
/** @internal */
export declare const Meter$outboundSchema: z.ZodType<Meter$Outbound, z.ZodTypeDef, Meter>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Meter$ {
    /** @deprecated use `Meter$inboundSchema` instead. */
    const inboundSchema: z.ZodType<Meter, z.ZodTypeDef, unknown>;
    /** @deprecated use `Meter$outboundSchema` instead. */
    const outboundSchema: z.ZodType<Meter$Outbound, z.ZodTypeDef, Meter>;
    /** @deprecated use `Meter$Outbound` instead. */
    type Outbound = Meter$Outbound;
}
export declare function meterToJSON(meter: Meter): string;
export declare function meterFromJSON(jsonString: string): SafeParseResult<Meter, SDKValidationError>;
//# sourceMappingURL=meter.d.ts.map