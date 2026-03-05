import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type CountAggregation = {
    func?: "count" | undefined;
};
/** @internal */
export declare const CountAggregation$inboundSchema: z.ZodType<CountAggregation, z.ZodTypeDef, unknown>;
/** @internal */
export type CountAggregation$Outbound = {
    func: "count";
};
/** @internal */
export declare const CountAggregation$outboundSchema: z.ZodType<CountAggregation$Outbound, z.ZodTypeDef, CountAggregation>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace CountAggregation$ {
    /** @deprecated use `CountAggregation$inboundSchema` instead. */
    const inboundSchema: z.ZodType<CountAggregation, z.ZodTypeDef, unknown>;
    /** @deprecated use `CountAggregation$outboundSchema` instead. */
    const outboundSchema: z.ZodType<CountAggregation$Outbound, z.ZodTypeDef, CountAggregation>;
    /** @deprecated use `CountAggregation$Outbound` instead. */
    type Outbound = CountAggregation$Outbound;
}
export declare function countAggregationToJSON(countAggregation: CountAggregation): string;
export declare function countAggregationFromJSON(jsonString: string): SafeParseResult<CountAggregation, SDKValidationError>;
//# sourceMappingURL=countaggregation.d.ts.map