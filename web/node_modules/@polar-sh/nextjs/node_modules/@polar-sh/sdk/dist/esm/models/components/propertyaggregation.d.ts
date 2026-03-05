import * as z from "zod";
import { ClosedEnum } from "../../types/enums.js";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export declare const Func: {
    readonly Sum: "sum";
    readonly Max: "max";
    readonly Min: "min";
    readonly Avg: "avg";
};
export type Func = ClosedEnum<typeof Func>;
export type PropertyAggregation = {
    func: Func;
    property: string;
};
/** @internal */
export declare const Func$inboundSchema: z.ZodNativeEnum<typeof Func>;
/** @internal */
export declare const Func$outboundSchema: z.ZodNativeEnum<typeof Func>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace Func$ {
    /** @deprecated use `Func$inboundSchema` instead. */
    const inboundSchema: z.ZodNativeEnum<{
        readonly Sum: "sum";
        readonly Max: "max";
        readonly Min: "min";
        readonly Avg: "avg";
    }>;
    /** @deprecated use `Func$outboundSchema` instead. */
    const outboundSchema: z.ZodNativeEnum<{
        readonly Sum: "sum";
        readonly Max: "max";
        readonly Min: "min";
        readonly Avg: "avg";
    }>;
}
/** @internal */
export declare const PropertyAggregation$inboundSchema: z.ZodType<PropertyAggregation, z.ZodTypeDef, unknown>;
/** @internal */
export type PropertyAggregation$Outbound = {
    func: string;
    property: string;
};
/** @internal */
export declare const PropertyAggregation$outboundSchema: z.ZodType<PropertyAggregation$Outbound, z.ZodTypeDef, PropertyAggregation>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace PropertyAggregation$ {
    /** @deprecated use `PropertyAggregation$inboundSchema` instead. */
    const inboundSchema: z.ZodType<PropertyAggregation, z.ZodTypeDef, unknown>;
    /** @deprecated use `PropertyAggregation$outboundSchema` instead. */
    const outboundSchema: z.ZodType<PropertyAggregation$Outbound, z.ZodTypeDef, PropertyAggregation>;
    /** @deprecated use `PropertyAggregation$Outbound` instead. */
    type Outbound = PropertyAggregation$Outbound;
}
export declare function propertyAggregationToJSON(propertyAggregation: PropertyAggregation): string;
export declare function propertyAggregationFromJSON(jsonString: string): SafeParseResult<PropertyAggregation, SDKValidationError>;
//# sourceMappingURL=propertyaggregation.d.ts.map